import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { request } from "node:http";
import type { Socket } from "node:net";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { setImmediate } from "node:timers/promises";
import { Auth } from "../src/auth.mjs";
import { Attachments } from "../src/attachments.mjs";
import { Visibility } from "../src/visibility.mjs";
import { createApplicationServer } from "../src/http.mjs";
import { State } from "../src/state.mjs";
import {
  BYTES,
  INPUT,
  OWNER,
  MemoryObjects,
  expectAppError,
  createFixture,
} from "./support.mjs";

await test("sessions survive reopen and expiry cleanup uses its index", async (t) => {
  const { state, states, options } = await createFixture(t);
  const now = Date.now();
  const tokenHash = "a".repeat(64);
  await state.call("addSession", { accountId: OWNER, tokenHash, now });
  const session = await state.call("session", { tokenHash, now });
  assert.ok(session);
  await state.close();

  const reopened = await State.open(options);
  states.push(reopened);
  assert.deepEqual(await reopened.call("session", { tokenHash, now }), session);
  await reopened.close();

  const db = new DatabaseSync(options.path);

  try {
    assert.equal(db.prepare("PRAGMA user_version").get()?.user_version, 1);

    const plan = db
      .prepare("EXPLAIN QUERY PLAN DELETE FROM sessions WHERE expires_at <= ?")
      .all(now);

    assert.ok(
      plan.some((row) => String(row.detail).includes("sessions_expiry")),
    );

    assert.ok(
      plan.every((row) => !String(row.detail).includes("SCAN sessions")),
    );
  } finally {
    db.close();
  }

  const current = await State.open(options);
  states.push(current);

  await current.call("addSession", {
    accountId: OWNER,
    tokenHash: "b".repeat(64),
    now: now + 8 * 60 * 60 * 1000,
  });

  assert.equal(await current.call("session", { tokenHash, now }), undefined);
});

await test("local password sign-in uses durable revocable sessions and distinguishes accounts from storage authority", async (t) => {
  const { state, states, options } = await createFixture(t);
  const auth = new Auth(state);
  await auth.addAccount("alice", "correct local password");

  await assert.rejects(
    auth.login("alice", "wrong local password"),
    expectAppError("unauthorized"),
  );

  await assert.rejects(
    auth.login("nobody", "correct local password"),
    expectAppError("unauthorized"),
  );

  const token = await auth.login("alice", "correct local password");
  const account = await auth.requireSession(token);
  assert.equal(account.username, "alice");

  await assert.rejects(
    auth.requireSession("storage-access-key"),
    expectAppError("unauthorized"),
  );

  await state.close();
  const reopened = await State.open(options);
  states.push(reopened);
  const restarted = new Auth(reopened);
  assert.deepEqual(await restarted.requireSession(token), account);
  await restarted.logout(token);

  await assert.rejects(
    restarted.requireSession(token),
    expectAppError("unauthorized"),
  );
});

await test("HTTP shutdown retains disconnected finalization until the admitted app transaction settles", async (t) => {
  const { state, options } = await createFixture(t);
  const token = "a".repeat(64);

  await state.call("addSession", {
    accountId: OWNER,
    tokenHash: createHash("sha256").update(token).digest("hex"),
    now: Date.now(),
  });

  const storage = new MemoryObjects();
  const release = Promise.withResolvers<void>();
  storage.holdVerification = release.promise;

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const issued = await app.create(OWNER, INPUT);
  storage.objects.set(options.prefix + issued.attemptId, BYTES);

  const application = await createApplicationServer(
    new Auth(state),
    app,
    "http://app.owned.invalid",
    "http://storage.owned.invalid",
  );

  const { server } = application;

  try {
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const address = server.address();
    assert.ok(address && typeof address !== "string");

    const connected = new Promise<Socket>((resolve) =>
      server.once("connection", resolve),
    );

    const req = request({
      hostname: "127.0.0.1",
      port: address.port,
      path: `/api/attempts/${issued.attemptId}/finalize`,
      method: "POST",
      headers: {
        host: "app.owned.invalid",
        origin: "http://app.owned.invalid",
        "x-attachment-request": "1",
        "content-type": "application/json",
        "content-length": "2",
        cookie: `attachment-session=${token}`,
      },
    });

    const disconnected = once(req, "error");
    req.end("{}");
    const socket = await connected;
    await storage.verified.promise;
    const socketClosed = once(socket, "close");
    req.destroy(new Error("owned client disconnect during verification"));
    await disconnected;
    await socketClosed;

    assert.equal(
      (
        await state.call("attempt", {
          attemptId: issued.attemptId,
          accountId: OWNER,
        })
      ).status,
      "pending",
    );

    let settled = false;
    const drained = application.close();
    assert.equal(application.close(), drained);

    const closing = drained.then(() => {
      settled = true;
    });

    await setImmediate();
    assert.equal(server.listening, false);

    assert.equal(
      settled,
      false,
      "closed sockets do not settle admitted app work",
    );

    release.resolve();
    await closing;
    assert.equal(settled, true);

    assert.equal(
      (
        await state.call("attempt", {
          attemptId: issued.attemptId,
          accountId: OWNER,
        })
      ).status,
      "attached",
    );

    assert.deepEqual(
      storage.objects.get(options.prefix + issued.attemptId),
      BYTES,
    );
  } finally {
    release.resolve();
    await application.close();
  }
});

await test("HTTP accounts protect attempt issuance, cross-user finalization/download and session logout without exposing secrets", async (t) => {
  const { state, options } = await createFixture(t);
  const auth = new Auth(state);
  await auth.addAccount("alice", "alice private password");
  await auth.addAccount("bruno", "bruno private password");
  const storage = new MemoryObjects();

  const app = new Attachments(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
  );

  const origin = "http://app.owned.invalid";

  const visibility = new Visibility(
    state,
    storage,
    options.bucket,
    options.prefix,
    true,
    true,
  );

  const application = await createApplicationServer(
    auth,
    app,
    origin,
    "http://storage.owned.invalid",
    visibility,
  );

  const { server } = application;

  try {
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const address = server.address();
    assert.ok(address && typeof address !== "string");

    const send = (
      path: string,
      body?: unknown,
      cookie?: string,
      extra: Record<string, string> = {},
    ) =>
      new Promise<{
        status: number;
        headers: Record<string, string | string[] | undefined>;
        text: string;
      }>((resolve, reject) => {
        const bytes =
          body === undefined ? undefined : Buffer.from(JSON.stringify(body));

        const req = request(
          {
            hostname: "127.0.0.1",
            port: address.port,
            path,
            method: bytes ? "POST" : "GET",
            headers: {
              host: "app.owned.invalid",
              origin,
              "x-attachment-request": "1",
              ...(bytes
                ? {
                    "content-type": "application/json",
                    "content-length": String(bytes.length),
                  }
                : {}),
              ...(cookie ? { cookie } : {}),
              ...extra,
            },
          },
          (res) => {
            let text = "";
            res.setEncoding("utf8");

            res.on("data", (chunk: string) => {
              text += chunk;
            });

            res.once("end", () =>
              resolve({
                status: res.statusCode ?? 0,
                headers: res.headers,
                text,
              }),
            );

            res.once("error", reject);
          },
        );

        req.setTimeout(10_000, () =>
          req.destroy(new Error("request watchdog")),
        );

        req.once("error", reject);
        req.end(bytes);
      });

    const denied = await send("/api/attempts", INPUT);
    assert.equal(denied.status, 401);
    assert.deepEqual(JSON.parse(denied.text), { error: "unauthorized" });

    const signedIn = await send("/api/login", {
      username: "alice",
      password: "alice private password",
    });

    assert.equal(signedIn.status, 200);
    const cookie = signedIn.headers["set-cookie"]?.[0]?.split(";")[0];
    assert.ok(cookie);

    assert.match(
      String(signedIn.headers["set-cookie"]),
      /HttpOnly; SameSite=Strict/u,
    );

    assert.equal(signedIn.text.includes("password"), false);

    const otherLogin = await send("/api/login", {
      username: "bruno",
      password: "bruno private password",
    });

    const otherCookie = otherLogin.headers["set-cookie"]?.[0]?.split(";")[0];
    assert.ok(otherCookie);

    assert.equal(
      (
        await send("/api/attempts", INPUT, cookie, {
          origin: "https://attacker.invalid",
        })
      ).status,
      401,
    );

    assert.equal(
      (
        await send("/api/attempts", INPUT, cookie, {
          "x-attachment-request": "",
        })
      ).status,
      401,
    );

    assert.equal(
      (await send("/api/attempts", INPUT, cookie, { host: "attacker.invalid" }))
        .status,
      400,
    );

    const created = await send("/api/attempts", INPUT, cookie);
    assert.equal(created.status, 201);
    assert.equal(created.headers["cache-control"], "no-store");
    assert.equal(created.headers["referrer-policy"], "no-referrer");
    const issued = JSON.parse(created.text) as { attemptId: string };

    assert.equal(
      (
        await send(
          `/api/attempts/${issued.attemptId}/finalize`,
          {},
          otherCookie,
        )
      ).status,
      404,
    );

    assert.equal(
      (
        await send(
          `/api/attempts/${issued.attemptId}/download`,
          {},
          otherCookie,
        )
      ).status,
      404,
    );

    const publication = `/api/attachments/${issued.attemptId}/visibility`;

    assert.equal(
      (await send(publication, { visibility: "public" }, cookie)).status,
      404,
    );

    storage.objects.set(options.prefix + issued.attemptId, BYTES);

    assert.equal(
      (await send(`/api/attempts/${issued.attemptId}/finalize`, {}, cookie))
        .status,
      200,
    );

    for (const [identity, headers, status] of [
      [undefined, {}, 401],
      [otherCookie, {}, 404],
      [cookie, { origin: "https://attacker.invalid" }, 401],
      [cookie, { "x-attachment-request": "" }, 401],
    ] as const) {
      assert.equal(
        (await send(publication, { visibility: "public" }, identity, headers))
          .status,
        status,
      );
    }

    assert.equal(
      (await send(publication, { visibility: "public", tags: [] }, cookie))
        .status,
      400,
    );

    const published = await send(publication, { visibility: "public" }, cookie);
    assert.equal(published.status, 200);

    assert.equal(
      (JSON.parse(published.text) as { publication: string }).publication,
      "pending",
    );

    assert.equal(
      storage.visibility.size,
      0,
      "the HTTP request persists intent before any storage mutation",
    );

    await visibility.reconcile();

    assert.equal(
      storage.visibility.get(options.prefix + issued.attemptId),
      "public",
    );

    const session = await send("/api/session", undefined, cookie);

    assert.deepEqual(JSON.parse(session.text), {
      username: "alice",
      publicAttachments: true,
      postDemo: false,
    });

    assert.equal((await send("/api/logout", {}, cookie)).status, 200);
    assert.equal((await send("/api/session", undefined, cookie)).status, 401);
  } finally {
    await application.close();
  }
});
