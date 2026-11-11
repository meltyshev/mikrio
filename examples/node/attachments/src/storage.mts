import { createHash } from "node:crypto";
import { Readable } from "node:stream";
import {
  DeleteObjectCommand,
  GetObjectCommand,
  ListObjectsV2Command,
  NoSuchKey,
  PutObjectCommand,
  PutObjectTaggingCommand,
  S3Client,
  type S3ClientConfig,
} from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import { createPresignedPost } from "@aws-sdk/s3-presigned-post";
import type { PostForm } from "./post-demo.mjs";
import {
  AppError,
  MAX_FILE_BYTES,
  RECONCILIATION_PAGE_SIZE,
  type Attempt,
  type Visibility,
} from "./model.mjs";

export interface Upload {
  readonly url: string;
  readonly headers: Readonly<Record<string, string>>;
}

export interface ObjectLocation {
  readonly bucket: string;
  readonly key: string;
}

export interface ObjectPage {
  readonly keys: readonly string[];
  readonly nextContinuationToken?: string;
}

/** The example owns this SDK boundary; the database owns all attachment state. */
export interface ObjectStorage {
  upload(attempt: Attempt): Promise<Upload>;
  verify(attempt: Attempt): Promise<void>;
  download(object: ObjectLocation): Promise<string>;
  delete(object: ObjectLocation): Promise<void>;
  setVisibility(object: ObjectLocation, visibility: Visibility): Promise<void>;
  list(
    bucket: string,
    prefix: string,
    continuationToken?: string,
  ): Promise<ObjectPage>;
}

export interface StorageOptions {
  endpoint: string;
  region: string;
  uploader: { accessKeyId: string; secretAccessKey: string };
  backend: { accessKeyId: string; secretAccessKey: string };
}

/** Owns SDK clients; close after all uploads, reads and maintenance settle. */
export class S3Storage implements ObjectStorage {
  readonly #uploader: S3Client;
  readonly #backend: S3Client;

  constructor(options: StorageOptions) {
    const shared: S3ClientConfig = {
      endpoint: options.endpoint,
      region: options.region,
      forcePathStyle: true,
      maxAttempts: 2,
      requestHandler: { connectionTimeout: 5000, requestTimeout: 30_000 },
    };

    this.#uploader = new S3Client({ ...shared, credentials: options.uploader });
    this.#backend = new S3Client({ ...shared, credentials: options.backend });
  }

  async upload(attempt: Attempt): Promise<Upload> {
    const encodedFilename = encodeURIComponent(attempt.filename).replace(
      /[!'()*]/gu,
      (character) => "%" + character.charCodeAt(0).toString(16).toUpperCase(),
    );

    const headers = {
      "if-none-match": "*",
      "content-type": "text/plain; charset=utf-8",
      "content-disposition": `attachment; filename*=UTF-8''${encodedFilename}`,
    };

    const expiresIn = Math.floor((attempt.expiresAt - Date.now()) / 1000);

    if (expiresIn < 1 || expiresIn > 300) {
      throw new AppError("conflict");
    }

    const url = await getSignedUrl(
      this.#uploader,
      new PutObjectCommand({
        Bucket: attempt.bucket,
        Key: attempt.key,
        IfNoneMatch: "*",
        ContentLength: attempt.contentLength,
        ContentType: headers["content-type"],
        ContentDisposition: headers["content-disposition"],
        ChecksumSHA256: Buffer.from(attempt.sha256, "hex").toString("base64"),
      }),
      {
        expiresIn,
        signableHeaders: new Set([...Object.keys(headers), "content-length"]),
      },
    );

    const signedHeaders = new Set(
      new URL(url).searchParams.get("X-Amz-SignedHeaders")?.split(";"),
    );

    const hasUnsignedHeaders = [...Object.keys(headers), "content-length"].some(
      (header) => !signedHeaders.has(header),
    );

    if (hasUnsignedHeaders) {
      throw new AppError("unavailable");
    }

    // A File/Blob supplies its own Content-Length. Browser code must not try to
    // set that forbidden request header; the signed value still binds its size.
    return { url, headers };
  }

  async verify(attempt: Attempt): Promise<void> {
    const object = await this.#backend
      .send(
        new GetObjectCommand({ Bucket: attempt.bucket, Key: attempt.key }),
        {
          abortSignal: AbortSignal.timeout(30_000),
        },
      )
      .catch((error: unknown) => {
        if (error instanceof NoSuchKey) {
          throw new AppError("notFound", { cause: error });
        }

        throw error;
      });

    const body = object.Body;

    if (!(body instanceof Readable)) {
      throw new AppError("unavailable");
    }

    try {
      if (object.ContentLength !== attempt.contentLength) {
        throw new AppError("invalid");
      }

      await verifyText(body, attempt);
    } finally {
      body.destroy();
    }
  }

  async download(object: ObjectLocation): Promise<string> {
    return getSignedUrl(
      this.#backend,
      new GetObjectCommand({ Bucket: object.bucket, Key: object.key }),
      { expiresIn: 60 },
    );
  }

  async createPostForm(
    object: ObjectLocation,
    status: "200" | "201" | "204" | "303",
    redirect: string,
  ): Promise<PostForm> {
    const fields = {
      "Content-Type": "text/plain; charset=utf-8",
      "Content-Disposition": "attachment",
      ...(status === "303"
        ? { success_action_redirect: redirect }
        : { success_action_status: status }),
    };

    return createPresignedPost(this.#backend, {
      Bucket: object.bucket,
      Key: object.key,
      Expires: 300,
      Fields: fields,
      Conditions: [
        ["content-length-range", 0, MAX_FILE_BYTES],
        ...Object.entries(fields).map(([name, value]) => ({ [name]: value })),
      ],
    });
  }

  async delete(object: ObjectLocation): Promise<void> {
    await this.#backend.send(
      new DeleteObjectCommand({ Bucket: object.bucket, Key: object.key }),
      {
        abortSignal: AbortSignal.timeout(30_000),
      },
    );
  }

  async setVisibility(
    object: ObjectLocation,
    visibility: Visibility,
  ): Promise<void> {
    // This app is the sole owner of its entire tag set. Never read/merge a tag set
    // owned by a different writer, and never give this credential to the browser.
    await this.#backend.send(
      new PutObjectTaggingCommand({
        Bucket: object.bucket,
        Key: object.key,
        Tagging: { TagSet: [{ Key: "visibility", Value: visibility }] },
      }),
      { abortSignal: AbortSignal.timeout(30_000) },
    );
  }

  async list(
    bucket: string,
    prefix: string,
    continuationToken?: string,
  ): Promise<ObjectPage> {
    const page = await this.#backend.send(
      new ListObjectsV2Command({
        Bucket: bucket,
        Prefix: prefix,
        MaxKeys: RECONCILIATION_PAGE_SIZE,
        ...(continuationToken === undefined
          ? {}
          : { ContinuationToken: continuationToken }),
      }),
      { abortSignal: AbortSignal.timeout(30_000) },
    );

    const keys = (page.Contents ?? []).map((object) => {
      if (object.Key === undefined || !object.Key.startsWith(prefix)) {
        throw new AppError("unavailable");
      }

      return object.Key;
    });

    if (keys.length > RECONCILIATION_PAGE_SIZE || page.CommonPrefixes?.length) {
      throw new AppError("unavailable");
    }

    if (page.IsTruncated !== true && page.IsTruncated !== false) {
      throw new AppError("unavailable");
    }

    if (!page.IsTruncated) {
      return { keys };
    }

    if (!page.NextContinuationToken) {
      throw new AppError("unavailable");
    }

    if (page.NextContinuationToken === continuationToken) {
      throw new AppError("unavailable");
    }

    return { keys, nextContinuationToken: page.NextContinuationToken };
  }

  close(): void {
    this.#uploader.destroy();
    this.#backend.destroy();
  }
}

/** Validate actual bytes, even if an uploader labels arbitrary data text/plain. */
export async function verifyText(
  body: AsyncIterable<Uint8Array>,
  attempt: Pick<Attempt, "contentLength" | "sha256">,
): Promise<void> {
  let receivedBytes = 0;
  const digest = createHash("sha256");
  const decoder = new TextDecoder("utf-8", { fatal: true });

  for await (const chunk of body) {
    receivedBytes += chunk.byteLength;

    const exceedsLength =
      receivedBytes > attempt.contentLength || receivedBytes > MAX_FILE_BYTES;

    if (exceedsLength) {
      throw new AppError("invalid");
    }

    digest.update(chunk);
    validateText(decodeText(decoder, chunk));
  }

  validateText(decodeText(decoder));

  if (receivedBytes !== attempt.contentLength) {
    throw new AppError("invalid");
  }

  if (digest.digest("hex") !== attempt.sha256) {
    throw new AppError("invalid");
  }
}

function decodeText(decoder: TextDecoder, chunk?: Uint8Array): string {
  try {
    return decoder.decode(chunk, { stream: chunk !== undefined });
  } catch (error) {
    if (error instanceof TypeError) {
      throw new AppError("invalid", { cause: error });
    }

    throw error;
  }
}

function validateText(value: string): void {
  // eslint-disable-next-line no-control-regex -- The selected text policy rejects binary control bytes.
  if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u.test(value)) {
    throw new AppError("invalid");
  }
}
