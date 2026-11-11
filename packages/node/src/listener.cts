import { inspect } from "node:util";
import { markAsUncloneable } from "node:worker_threads";
import { Call } from "./calls.cjs";
import { canceled, storageError, StorageError } from "./errors.cjs";
import { record, string } from "./inputs.cjs";
import type { CallOptions } from "./index.cjs";
import type { NativeServer, NativeStore } from "./native.cjs";

/** A detached signing identity; authorization may change independently. */
export interface Credentials {
  readonly accessKeyId: string;
  readonly secretAccessKey: string;
}

/** Listener-only options. Port zero asks the OS to assign an available port. */
export interface ListenOptions {
  host?: string | undefined;
  port?: number | undefined;
  publicEndpoint?: string | undefined;
  credentials?: Credentials | undefined;
}

class CredentialSnapshot implements Credentials {
  readonly accessKeyId: string;
  readonly #secret: string;

  constructor(value: Credentials) {
    this.accessKeyId = value.accessKeyId;
    this.#secret = value.secretAccessKey;
    Object.freeze(this);
  }

  get secretAccessKey(): string {
    return this.#secret;
  }

  [inspect.custom](): unknown {
    return { accessKeyId: this.accessKeyId, secretAccessKey: "[REDACTED]" };
  }
}

let fromNative: (native: NativeServer) => Server;

/** One owned listener. Close it explicitly before ending its host lifetime. */
export class Server {
  readonly #native: NativeServer;
  readonly #endpoint: string;
  readonly #region: string;
  readonly #credentials: Credentials;
  readonly #completion: Promise<unknown>;
  #closing: Promise<void> | undefined;

  static {
    fromNative = (native) => new Server(native);
  }

  private constructor(native: NativeServer) {
    this.#native = native;

    const info = native.info;
    this.#endpoint = info.endpoint;
    this.#region = info.region;
    this.#credentials = new CredentialSnapshot(info);
    markAsUncloneable(this);

    // NAPI's completion wait retains the environment, not the Server owner.
    // Capture failures as values until explicit close observes them. Neither
    // callback closes over this wrapper, so GC can request fallback shutdown.
    this.#completion = native.closed().then(
      () => undefined,
      (error: unknown) => storageError(error),
    );
  }

  get endpoint(): string {
    return this.#endpoint;
  }

  get region(): string {
    return this.#region;
  }

  get credentials(): Credentials {
    return this.#credentials;
  }

  /** Stop HTTP work and await native settlement; direct store calls remain usable. */
  async close(): Promise<void> {
    let attempt: Promise<void> | undefined;

    try {
      attempt = this.#closing ?? this.#native.close();
      this.#closing = attempt;
      await attempt;

      const failure = await this.#completion;

      if (failure !== undefined) {
        throw storageError(failure);
      }
    } catch (error: unknown) {
      throw storageError(error);
    } finally {
      if (this.#closing === attempt) {
        this.#closing = undefined;
      }
    }
  }

  async [Symbol.asyncDispose](): Promise<void> {
    await this.close();
  }
}

function listenOptions(value: unknown): ListenOptions {
  if (value === undefined) {
    return {};
  }

  const fields = record(
    value,
    ["host", "port", "publicEndpoint", "credentials"],
    "ListenOptions",
  );

  const host = fields.host === undefined ? undefined : string(fields.host);

  const publicEndpoint =
    fields.publicEndpoint === undefined
      ? undefined
      : string(fields.publicEndpoint);

  if (fields.port !== undefined && typeof fields.port !== "number") {
    throw new TypeError("port must be a number or undefined");
  }

  let credentials: Credentials | undefined;

  if (fields.credentials !== undefined) {
    const input = record(
      fields.credentials,
      ["accessKeyId", "secretAccessKey"],
      "Credentials",
    );

    credentials = {
      accessKeyId: string(input.accessKeyId),
      secretAccessKey: string(input.secretAccessKey),
    };
  }

  return { host, port: fields.port, publicEndpoint, credentials };
}

export async function listen(
  native: NativeStore,
  input: ListenOptions | undefined,
  options: CallOptions | undefined,
): Promise<Server> {
  const fields = listenOptions(input);
  const call = new Call(options, false);

  try {
    const server = await native.listen(fields, call.cancellation);

    if (call.signal?.aborted) {
      const error = canceled(call.signal);

      try {
        await server.close();
      } catch (cleanup: unknown) {
        throw new AggregateError(
          [error, storageError(cleanup)],
          "Listen cancellation cleanup failed",
          { cause: cleanup },
        );
      }

      throw error;
    }

    return fromNative(server);
  } catch (error: unknown) {
    const converted = storageError(error);

    const wasCanceled =
      converted instanceof StorageError &&
      converted.code === "OperationCanceled";

    if (wasCanceled && call.signal?.aborted) {
      throw canceled(call.signal);
    }

    throw converted;
  } finally {
    call.close();
  }
}
