import { readFile, stat } from "node:fs/promises";
import { isAbsolute } from "node:path";
import {
  AppError,
  parseRecord,
  parseText,
  type StateOptions,
} from "./model.mjs";
import type { StorageOptions } from "./storage.mjs";

export interface Config {
  appOrigin: string;
  appPort: number;
  state: StateOptions;
  storage: StorageOptions;
  local?: { directory: string; port: number };
  publicAttachments?: boolean;
  postDemo?: { bucket: string; prefix: string };
}

/** Read and validate the private configuration file without opening its stores. */
export async function loadConfig(path: string): Promise<Config> {
  const metadata = await stat(path);

  const hasInvalidFile =
    metadata.size > 16_384 ||
    !metadata.isFile() ||
    (process.platform !== "win32" && (metadata.mode & 0o077) !== 0);

  if (hasInvalidFile) {
    throw new AppError("invalid");
  }

  const input: unknown = JSON.parse(await readFile(path, "utf8"));
  const config = parseRecord(input);

  if (config.publicAttachments !== undefined) {
    if (typeof config.publicAttachments !== "boolean") {
      throw new AppError("invalid");
    }
  }

  const state = parseRecord(config.state);
  const storage = parseRecord(config.storage);
  const appOrigin = parseOrigin(config.appOrigin);
  const prefix = parseText(state.prefix, 128);
  const bucket = parseText(state.bucket, 63);
  const statePath = parseText(state.path, 4096);

  if (!/^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$/u.test(bucket)) {
    throw new AppError("invalid");
  }

  if (!/^[a-z0-9-]+\/$/u.test(prefix)) {
    throw new AppError("invalid");
  }

  if (!isAbsolute(statePath)) {
    throw new AppError("invalid");
  }

  const result: Config = {
    appOrigin,
    appPort: parsePort(config.appPort),
    state: {
      path: statePath,
      storageId: parseText(state.storageId, 128),
      bucket,
      prefix,
    },
    storage: {
      endpoint: parseOrigin(storage.endpoint),
      region: parseText(storage.region, 64),
      uploader: parseCredentials(storage.uploader),
      backend: parseCredentials(storage.backend),
    },
  };

  if (config.publicAttachments !== undefined) {
    result.publicAttachments = config.publicAttachments;
  }

  if (config.postDemo !== undefined) {
    const demo = parseRecord(config.postDemo);
    const demoBucket = parseText(demo.bucket, 63);
    const demoPrefix = parseText(demo.prefix, 128);

    if (demoBucket === bucket) {
      throw new AppError("invalid");
    }

    if (!/^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$/u.test(demoBucket)) {
      throw new AppError("invalid");
    }

    if (!/^[a-z0-9-]+\/$/u.test(demoPrefix)) {
      throw new AppError("invalid");
    }

    result.postDemo = { bucket: demoBucket, prefix: demoPrefix };
  }

  if (config.local !== undefined) {
    const local = parseRecord(config.local);
    const directory = parseText(local.directory, 4096);

    if (!isAbsolute(directory)) {
      throw new AppError("invalid");
    }

    result.local = { directory, port: parsePort(local.port) };
  }

  return result;
}

function parseOrigin(value: unknown): string {
  const input = parseText(value, 2048);
  const url = new URL(input);

  if (url.origin !== input || url.username || url.password) {
    throw new AppError("invalid");
  }

  const hasAllowedTransport =
    url.protocol === "https:" ||
    (url.protocol === "http:" &&
      ["127.0.0.1", "[::1]", "localhost"].includes(url.hostname));

  if (!hasAllowedTransport) {
    throw new AppError("invalid");
  }

  return input;
}

function parsePort(value: unknown): number {
  if (typeof value !== "number") {
    throw new AppError("invalid");
  }

  if (!Number.isInteger(value) || value < 1 || value > 65535) {
    throw new AppError("invalid");
  }

  return value;
}

function parseCredentials(value: unknown): {
  accessKeyId: string;
  secretAccessKey: string;
} {
  const fields = parseRecord(value);

  return {
    accessKeyId: parseText(fields.accessKeyId, 128),
    secretAccessKey: parseText(fields.secretAccessKey, 256),
  };
}
