import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { TestContext } from "node:test";
import {
  AppError,
  RECONCILIATION_PAGE_SIZE,
  type Attempt,
  type ErrorCode,
  type StateOptions,
  type Visibility,
} from "../src/model.mjs";
import { State } from "../src/state.mjs";
import {
  verifyText,
  type ObjectStorage,
  type ObjectLocation,
  type ObjectPage,
  type Upload,
} from "../src/storage.mjs";

export const OWNER = "app-owner";
export const OTHER = "other-owner";
export const BYTES = Buffer.from("An independently verified attachment.\n");
export const INPUT = {
  filename: "notes.txt",
  contentLength: BYTES.length,
  sha256: createHash("sha256").update(BYTES).digest("hex"),
};

/** Match both the app error type and its stable code in rejection assertions. */
export function expectAppError(code: ErrorCode): (error: unknown) => boolean {
  return (error) => {
    assert.ok(error instanceof AppError);
    assert.equal(error.code, code);
    return true;
  };
}

/** Own app state and caller-registered dependencies; retain files after failure. */
export async function createFixture(t: TestContext): Promise<{
  options: StateOptions;
  state: State;
  states: State[];
  resources: AsyncDisposableStack;
}> {
  const root = await mkdtemp(join(tmpdir(), "mikrio-attachment-"));
  const states: State[] = [];
  const resources = new AsyncDisposableStack();

  t.after(async () => {
    let settled = false;

    try {
      await resources.disposeAsync();
      settled = true;
    } finally {
      if (t.passed && settled) {
        await rm(root, { recursive: true });
      } else {
        t.diagnostic(`retained failed application state: ${root}`);
      }
    }
  });

  // Callers add dependent resources to this stack so they close before state
  // and before its directory is removed. Test after-hooks run in registration order.
  resources.defer(async () => {
    const results = await Promise.allSettled(
      states.map((state) => state.close()),
    );

    const errors = results
      .filter((result) => result.status === "rejected")
      .map((result) => result.reason as unknown);

    if (errors.length) {
      throw new AggregateError(errors, "state cleanup failed");
    }
  });

  const options = {
    path: join(root, "app.sqlite3"),
    storageId: "owned-test-dataset",
    bucket: "attachments",
    prefix: "private/",
  };

  const state = await State.open({ ...options, initialize: true });
  states.push(state);

  for (const accountId of [OWNER, OTHER]) {
    await state.call("addAccount", {
      accountId,
      username: accountId,
      salt: "00".repeat(16),
      passwordHash: "00".repeat(32),
    });
  }

  return { options, state, states, resources };
}

/** Owns only the network boundary; all transaction tests use real SQLite files. */
export class MemoryObjects implements ObjectStorage {
  readonly objects = new Map<string, Buffer>();
  readonly deleted: string[] = [];
  readonly uploaded: Attempt[] = [];
  readonly verified = Promise.withResolvers<void>();
  holdVerification: Promise<void> = Promise.resolve();
  loseDeleteResponse = false;
  readonly visibility = new Map<string, Visibility>();

  setVisibility(object: ObjectLocation, visibility: Visibility): Promise<void> {
    if (!this.objects.has(object.key)) {
      return Promise.reject(new AppError("notFound"));
    }

    this.visibility.set(object.key, visibility);
    return Promise.resolve();
  }

  upload(attempt: Attempt): Promise<Upload> {
    this.uploaded.push(attempt);

    return Promise.resolve({
      url: "https://owned.invalid/" + attempt.key,
      headers: { "if-none-match": "*" },
    });
  }

  async verify(attempt: Attempt): Promise<void> {
    const bytes = this.objects.get(attempt.key);

    if (!bytes) {
      throw new AppError("notFound");
    }

    await verifyText(readChunks(bytes), attempt);
    this.verified.resolve();
    await this.holdVerification;
  }

  download(object: ObjectLocation): Promise<string> {
    return Promise.resolve("https://owned.invalid/" + object.key);
  }

  delete(object: ObjectLocation): Promise<void> {
    this.deleted.push(object.key);
    this.objects.delete(object.key);

    if (this.loseDeleteResponse) {
      return Promise.reject(new Error("owned lost reply"));
    }

    return Promise.resolve();
  }

  list(
    _bucket: string,
    prefix: string,
    continuationToken?: string,
  ): Promise<ObjectPage> {
    const keys = [...this.objects.keys()]
      .filter(
        (key) =>
          key.startsWith(prefix) &&
          (continuationToken === undefined || key > continuationToken),
      )
      .sort();

    const page = keys.slice(0, RECONCILIATION_PAGE_SIZE);
    const next = page.at(-1);

    return Promise.resolve({
      keys: page,
      ...(keys.length > page.length && next !== undefined
        ? { nextContinuationToken: next }
        : {}),
    });
  }
}

/** Yield small chunks so content checks exercise boundaries within UTF-8 text. */
export async function* readChunks(bytes: Buffer): AsyncGenerator<Buffer> {
  for (let offset = 0; offset < bytes.length; offset += 3) {
    yield Promise.resolve(bytes.subarray(offset, offset + 3));
  }
}
