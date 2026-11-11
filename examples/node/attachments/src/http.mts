import {
  createServer,
  type IncomingMessage,
  type Server,
  type ServerResponse,
} from "node:http";
import { readFile } from "node:fs/promises";
import { AppError, parseRecord, type ErrorCode } from "./model.mjs";
import type { Auth } from "./auth.mjs";
import type { Attachments } from "./attachments.mjs";
import type { Visibility } from "./visibility.mjs";
import type { PostDemo } from "./post-demo.mjs";

const STATUS: Record<ErrorCode, number> = {
  invalid: 400,
  unauthorized: 401,
  notFound: 404,
  conflict: 409,
  unavailable: 503,
};

interface ApplicationServer {
  readonly server: Server;
  close(): Promise<void>;
}

/** Create an unbound server; close it before releasing auth and storage state. */
export async function createApplicationServer(
  auth: Auth,
  attachments: Attachments,
  origin: string,
  storageEndpoint: string,
  visibility?: Visibility,
  postDemo?: PostDemo,
): Promise<ApplicationServer> {
  const expected = new URL(origin);
  const secure = expected.protocol === "https:";

  const cookieName = secure
    ? "__Host-attachment-session"
    : "attachment-session";

  const assets = new Map([
    [
      "/",
      {
        body: await readFile(
          new URL("../../public/index.html", import.meta.url),
        ),
        type: "text/html; charset=utf-8",
      },
    ],
    [
      "/app.js",
      {
        body: await readFile(new URL("../../public/app.js", import.meta.url)),
        type: "text/javascript; charset=utf-8",
      },
    ],
    [
      "/style.css",
      {
        body: await readFile(
          new URL("../../public/style.css", import.meta.url),
        ),
        type: "text/css; charset=utf-8",
      },
    ],
  ]);

  const handlers = new Set<Promise<void>>();

  if (postDemo) {
    for (const [path, type] of [
      ["post.html", "text/html; charset=utf-8"],
      ["post.js", "text/javascript; charset=utf-8"],
    ] as const) {
      assets.set("/" + path, {
        body: await readFile(new URL("../../public/" + path, import.meta.url)),
        type,
      });
    }
  }

  let stopping = false;
  let closing: Promise<void> | undefined;

  const server = createServer(
    {
      maxHeaderSize: 8192,
      headersTimeout: 10_000,
      requestTimeout: 15_000,
      keepAliveTimeout: 5000,
    },
    (request, response) => {
      response.setHeader("cache-control", "no-store");
      response.setHeader("referrer-policy", "no-referrer");
      response.setHeader("x-content-type-options", "nosniff");

      response.setHeader(
        "content-security-policy",
        "default-src 'self'; connect-src 'self' " +
          new URL(storageEndpoint).origin +
          "; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
      );

      if (stopping || handlers.size >= 32) {
        reply(response, 503, { error: "unavailable" });
        request.resume();
        return;
      }

      const handler = route(request, response)
        .catch((error: unknown) => {
          const code = error instanceof AppError ? error.code : "unavailable";

          if (!response.headersSent && !response.destroyed) {
            reply(response, STATUS[code], { error: code });
          } else {
            response.destroy();
          }

          // Never log a request, SDK error, signed URL, cookie, password or body.
          request.resume();
        })
        .finally(() => {
          handlers.delete(handler);
        });

      handlers.add(handler);
    },
  );

  server.maxConnections = 64;
  server.maxRequestsPerSocket = 100;
  return { server, close };

  function close(): Promise<void> {
    if (closing) {
      return closing;
    }

    stopping = true;

    const closed = server.listening
      ? new Promise<void>((resolve, reject) =>
          server.close((error) => {
            if (error) {
              reject(error);
            } else {
              resolve();
            }
          }),
        )
      : Promise.resolve();

    // Socket closure does not cancel SDK/SQL work from a disconnected caller.
    // Keep its dependencies alive until every admitted handler has settled.
    closing = Promise.allSettled([closed, ...handlers]).then((results) => {
      const failures = results
        .filter((result) => result.status === "rejected")
        .map((result) => result.reason as unknown);

      if (failures.length) {
        throw new AggregateError(
          failures,
          "application requests failed to settle",
        );
      }
    });

    return closing;
  }

  async function route(
    request: IncomingMessage,
    response: ServerResponse,
  ): Promise<void> {
    if (request.headers.host !== expected.host) {
      throw new AppError("invalid");
    }

    const path = request.url ?? "";

    const assetPath =
      postDemo && path.startsWith("/post.html?") ? "/post.html" : path;

    const asset = assets.get(assetPath);

    if (request.method === "GET" && asset) {
      response.writeHead(200, { "content-type": asset.type });
      response.end(asset.body);
      return;
    }

    if (!path.startsWith("/api/")) {
      throw new AppError("notFound");
    }

    const hasInvalidOrigin =
      request.headers["x-attachment-request"] !== "1" ||
      (request.headers.origin !== undefined &&
        request.headers.origin !== origin) ||
      (request.method !== "GET" && request.headers.origin !== origin);

    if (hasInvalidOrigin) {
      throw new AppError("unauthorized");
    }

    const token = readSessionCookie(request, cookieName);

    if (path === "/api/login" && request.method === "POST") {
      const input = parseRecord(await readJsonBody(request));
      const session = await auth.login(input.username, input.password);

      response.setHeader(
        "set-cookie",
        formatSessionCookie(session, 8 * 60 * 60),
      );

      reply(response, 200, {});
      return;
    }

    const account = await auth.requireSession(token);

    if (path === "/api/session" && request.method === "GET") {
      reply(response, 200, {
        username: account.username,
        publicAttachments: visibility?.enabled === true,
        postDemo: postDemo !== undefined,
      });

      return;
    }

    if (path === "/api/logout" && request.method === "POST") {
      await readJsonBody(request);

      if (token) {
        await auth.logout(token);
      }

      response.setHeader("set-cookie", formatSessionCookie("", 0));
      reply(response, 200, {});
      return;
    }

    if (path === "/api/attachments" && request.method === "GET") {
      reply(response, 200, {
        attachments: await auth.state.call("attachments", {
          accountId: account.accountId,
        }),
      });

      return;
    }

    if (path === "/api/attempts" && request.method === "POST") {
      reply(
        response,
        201,
        await attachments.create(
          account.accountId,
          await readJsonBody(request),
        ),
      );

      return;
    }

    if (path === "/api/post-demo/forms" && request.method === "POST") {
      if (!postDemo) {
        throw new AppError("notFound");
      }

      reply(
        response,
        201,
        await postDemo.issue(account.accountId, await readJsonBody(request)),
      );

      return;
    }

    const demo =
      /^\/api\/post-demo\/([0-9a-f.-]{1,64})\/(download|delete)$/u.exec(path);

    if (request.method === "POST" && demo?.[1]) {
      if (!postDemo) {
        throw new AppError("notFound");
      }

      await readJsonBody(request);

      if (demo[2] === "download") {
        reply(response, 200, {
          url: await postDemo.download(account.accountId, demo[1]),
        });
      } else {
        await postDemo.delete(account.accountId, demo[1]);
        reply(response, 200, {});
      }

      return;
    }

    const publication =
      /^\/api\/attachments\/([0-9a-f-]{36})\/visibility$/u.exec(path);

    if (request.method === "POST" && publication?.[1]) {
      if (!visibility) {
        throw new AppError("unavailable");
      }

      reply(
        response,
        200,
        await visibility.change(
          account.accountId,
          publication[1],
          await readJsonBody(request),
        ),
      );

      return;
    }

    const operation =
      /^\/api\/attempts\/([0-9a-f-]{36})\/(finalize|download)$/u.exec(path);

    if (request.method !== "POST" || !operation?.[1]) {
      throw new AppError("notFound");
    }

    await readJsonBody(request);
    const attemptId = operation[1];

    const output =
      operation[2] === "finalize"
        ? await attachments.finalize(account.accountId, attemptId)
        : { url: await attachments.download(account.accountId, attemptId) };

    reply(response, 200, output);
  }

  function formatSessionCookie(token: string, maxAgeSeconds: number): string {
    return `${cookieName}=${token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=${maxAgeSeconds}${secure ? "; Secure" : ""}`;
  }
}

async function readJsonBody(request: IncomingMessage): Promise<unknown> {
  if (request.headers["content-type"] !== "application/json") {
    throw new AppError("invalid");
  }

  let receivedBytes = 0;
  const chunks: Buffer[] = [];
  const timeout = setTimeout(() => request.destroy(), 10_000);

  try {
    for await (const chunk of request) {
      if (!Buffer.isBuffer(chunk)) {
        throw new AppError("invalid");
      }

      receivedBytes += chunk.length;

      if (receivedBytes > 8192) {
        throw new AppError("invalid");
      }

      chunks.push(chunk);
    }

    const value: unknown = JSON.parse(
      new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks)),
    );

    return value;
  } catch (error) {
    if (error instanceof SyntaxError || error instanceof TypeError) {
      throw new AppError("invalid");
    }

    throw error;
  } finally {
    clearTimeout(timeout);
  }
}

function readSessionCookie(
  request: IncomingMessage,
  name: string,
): string | undefined {
  const matches =
    request.headers.cookie
      ?.split(";")
      .map((part) => part.trim())
      .filter((part) => part.startsWith(name + "=")) ?? [];

  if (matches.length > 1) {
    throw new AppError("unauthorized");
  }

  return matches[0]?.slice(name.length + 1);
}

function reply(response: ServerResponse, status: number, body: unknown): void {
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
  });

  response.end(JSON.stringify(body));
}
