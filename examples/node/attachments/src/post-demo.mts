import { randomUUID } from "node:crypto";
import {
  AppError,
  MAX_FILE_BYTES,
  RECONCILIATION_PAGE_SIZE,
  UPLOAD_LIFETIME_MS,
  parseRecord,
  parseText,
  validateTimestamp,
} from "./model.mjs";
import type { ObjectLocation, ObjectStorage } from "./storage.mjs";

export interface PostForm {
  readonly url: string;
  readonly fields: Readonly<Record<string, string>>;
}

export interface PostStorage extends Pick<
  ObjectStorage,
  "download" | "delete" | "list"
> {
  /** Sign a reusable form; uploading remains the caller's separate request. */
  createPostForm(
    object: ObjectLocation,
    status: "200" | "201" | "204" | "303",
    redirect: string,
  ): Promise<PostForm>;
}

export const DEMO_RETENTION_MS = 15 * 60 * 1000;

const DEMO_ID =
  /^([0-9]{1,13})\.([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})$/u;

/** Ephemeral objects have account-scoped keys, never attachment records. */
export class PostDemo {
  #cleanup: Promise<Readonly<{ deleted: number; failed: number }>> | undefined;
  #continuationToken: string | undefined;

  constructor(
    readonly storage: PostStorage,
    readonly bucket: string,
    readonly prefix: string,
    readonly appOrigin: string,
    readonly cleanupAccepted: boolean,
  ) {}

  async issue(
    accountId: string,
    input: unknown,
  ): Promise<{
    readonly id: string;
    readonly expiresAt: number;
    readonly cleanupAt: number;
    readonly maxFileBytes: number;
    readonly form: PostForm;
  }> {
    if (!this.cleanupAccepted) {
      throw new AppError("unavailable");
    }

    const fields = parseRecord(input);
    const status = fields.status ?? "201";

    const hasInvalidStatus =
      status !== "200" &&
      status !== "201" &&
      status !== "204" &&
      status !== "303";

    if (hasInvalidStatus) {
      throw new AppError("invalid");
    }

    if (Object.keys(fields).some((key) => key !== "status")) {
      throw new AppError("invalid");
    }

    const now = validateTimestamp(Date.now());
    const cleanupAt = validateTimestamp(now + DEMO_RETENTION_MS);
    const demoId = `${cleanupAt}.${randomUUID()}`;

    const form = await this.storage.createPostForm(
      this.#createObjectLocation(accountId, demoId),
      status,
      this.appOrigin + "/post.html",
    );

    return {
      id: demoId,
      expiresAt: now + UPLOAD_LIFETIME_MS,
      cleanupAt,
      maxFileBytes: MAX_FILE_BYTES,
      form,
    };
  }

  download(accountId: string, demoId: string): Promise<string> {
    return this.storage.download(this.#createObjectLocation(accountId, demoId));
  }

  delete(accountId: string, demoId: string): Promise<void> {
    // Deletion does not revoke the signed form. Recurring discovery catches
    // replay and transfers admitted before expiry but completed much later.
    return this.storage.delete(this.#createObjectLocation(accountId, demoId));
  }

  cleanup(
    now = Date.now(),
  ): Promise<Readonly<{ deleted: number; failed: number }>> {
    if (!this.cleanupAccepted) {
      return Promise.reject(new AppError("unavailable"));
    }

    validateTimestamp(now);

    this.#cleanup ??= this.#clean(now)
      .catch((error: unknown) => {
        this.#continuationToken = undefined;
        throw error;
      })
      .finally(() => {
        this.#cleanup = undefined;
      });

    return this.#cleanup;
  }

  async #clean(
    now: number,
  ): Promise<Readonly<{ deleted: number; failed: number }>> {
    const page = await this.storage.list(
      this.bucket,
      this.prefix,
      this.#continuationToken,
    );

    if (page.keys.length > RECONCILIATION_PAGE_SIZE) {
      throw new AppError("unavailable");
    }

    if (page.keys.some((key) => !key.startsWith(this.prefix))) {
      throw new AppError("unavailable");
    }

    const hasInvalidContinuation =
      page.nextContinuationToken !== undefined &&
      (!page.nextContinuationToken ||
        page.nextContinuationToken === this.#continuationToken);

    if (hasInvalidContinuation) {
      throw new AppError("unavailable");
    }

    let deleted = 0;
    let failed = 0;

    for (const key of page.keys) {
      const relative = key.slice(this.prefix.length);
      const parts = relative.split("/");

      const parsedDemoId =
        parts.length === 2 ? DEMO_ID.exec(parts[1] ?? "") : null;

      // This explicitly accepted, exclusive prefix contains only disposable
      // demonstration objects. Unknown keys have no independent live owner.
      if (parsedDemoId?.[1] && Number(parsedDemoId[1]) > now) {
        continue;
      }

      try {
        await this.storage.delete({ bucket: this.bucket, key });
        deleted += 1;
      } catch {
        failed += 1;
      }
    }

    this.#continuationToken = page.nextContinuationToken;
    return { deleted, failed };
  }

  #createObjectLocation(accountId: string, demoId: string): ObjectLocation {
    if (!/^[a-zA-Z0-9-]+$/u.test(parseText(accountId, 128))) {
      throw new AppError("invalid");
    }

    if (!DEMO_ID.test(parseText(demoId, 64))) {
      throw new AppError("invalid");
    }

    return { bucket: this.bucket, key: this.prefix + accountId + "/" + demoId };
  }
}
