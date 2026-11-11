import { AppError, parseRecord, type Attachment } from "./model.mjs";
import type { State } from "./state.mjs";
import type { ObjectStorage } from "./storage.mjs";

/** One app owner serializes tag writes and revisits even acknowledged intent. */
export class Visibility {
  #running: Promise<Readonly<{ applied: number; failed: number }>> | undefined;

  constructor(
    readonly state: State,
    readonly storage: ObjectStorage,
    readonly bucket: string,
    readonly prefix: string,
    readonly enabled: boolean,
    readonly authoritative: boolean,
  ) {}

  async change(
    accountId: string,
    attemptId: string,
    input: unknown,
  ): Promise<Attachment> {
    if (!this.enabled || !this.authoritative) {
      throw new AppError("unavailable");
    }

    const fields = parseRecord(input);

    if (Object.keys(fields).length !== 1) {
      throw new AppError("invalid");
    }

    if (fields.visibility !== "public" && fields.visibility !== "private") {
      throw new AppError("invalid");
    }

    const attachment = await this.state.call("attachment", {
      accountId,
      attemptId,
    });

    if (!attachment) {
      throw new AppError("notFound");
    }

    this.#validateLocation(attachment.bucket, attachment.key, attemptId);

    return this.state.call("setVisibility", {
      accountId,
      attemptId,
      visibility: fields.visibility,
    });
  }

  reconcile(
    now = Date.now(),
  ): Promise<Readonly<{ applied: number; failed: number }>> {
    if (!this.enabled || !this.authoritative) {
      return Promise.reject(new AppError("unavailable"));
    }

    this.#running ??= this.#reconcile(now).finally(() => {
      this.#running = undefined;
    });

    return this.#running;
  }

  async #reconcile(
    now: number,
  ): Promise<Readonly<{ applied: number; failed: number }>> {
    const attemptIds = await this.state.call("claimVisibility", { now });
    let applied = 0;
    let failed = 0;

    for (const attemptId of attemptIds) {
      // Read current intent immediately before sending. No app transaction spans
      // the SDK call; a revision fences its acknowledgment, not the remote write.
      const work = await this.state.call("visibilityWork", { attemptId });
      this.#validateLocation(work.bucket, work.key, attemptId);
      let succeeded = false;

      try {
        await this.storage.setVisibility(work, work.visibility);
        succeeded = true;
        applied += 1;
      } catch {
        failed += 1;
      }

      await this.state.call("finishVisibility", {
        attemptId,
        revision: work.revision,
        succeeded,
        now,
      });
    }

    return { applied, failed };
  }

  #validateLocation(bucket: string, key: string, attemptId: string): void {
    if (bucket !== this.bucket || key !== this.prefix + attemptId) {
      throw new AppError("unavailable");
    }
  }
}
