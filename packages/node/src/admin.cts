import { inspect } from "node:util";
import type {
  PrincipalInfo,
  AccessKeyInfo,
  CreateAccessKeyOutput,
} from "./values.cjs";
import type { NativePrincipalInfo, NativeAccessKeyInfo } from "./native.cjs";

/** Policy text is explicit to callers and absent from inspection/serialization. */
export class StoredPolicy {
  readonly #document: string;

  constructor(document: string) {
    this.#document = document;
    Object.freeze(this);
  }

  get document(): string {
    return this.#document;
  }

  [inspect.custom](): unknown {
    return { document: "[REDACTED]" };
  }
}

export function principalInfo(value: NativePrincipalInfo): PrincipalInfo {
  return Object.freeze({ ...value, createdAt: new Date(value.createdAt) });
}

export function accessKeyInfo(value: NativeAccessKeyInfo): AccessKeyInfo {
  return Object.freeze({ ...value, createdAt: new Date(value.createdAt) });
}

/** Expose the new signing secret explicitly while redacting ordinary inspection. */
export class CreatedAccessKey implements CreateAccessKeyOutput {
  readonly accessKey: AccessKeyInfo;
  readonly #secret: string;

  constructor(accessKey: NativeAccessKeyInfo, secret: string) {
    this.accessKey = accessKeyInfo(accessKey);
    this.#secret = secret;
    Object.freeze(this);
  }

  get secretAccessKey(): string {
    return this.#secret;
  }

  [inspect.custom](): unknown {
    return { accessKey: this.accessKey, secretAccessKey: "[REDACTED]" };
  }
}
