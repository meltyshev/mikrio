import { randomUUID } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { Store, StorageError } from "mikrio";
import { State } from "./state.mjs";
import { AppError } from "./model.mjs";
import type { Config } from "./config.mjs";

/** Provision a fresh private demonstration through the supported package root. */
export async function initializeLocal(directory: string): Promise<string> {
  const root = resolve(directory);
  await mkdir(root, { mode: 0o700 });
  await using store = await Store.open({ directory: join(root, "store") });
  const bucket = "attachments";
  const prefix = "private/";
  const appOrigin = "http://127.0.0.1:3000";
  const postDemo = { bucket: "post-demonstration", prefix: "ephemeral/" };
  await store.createBucket({ bucket });

  await store.putBucketCors({
    bucket,
    corsRules: [
      {
        allowedOrigins: [appOrigin],
        allowedMethods: ["PUT", "GET", "HEAD"],
        allowedHeaders: [
          "if-none-match",
          "content-type",
          "content-disposition",
        ],
        exposeHeaders: ["ETag", "x-amz-checksum-sha256", "Content-Disposition"],
      },
    ],
  });

  await store.putPublicAccessBlock({
    bucket,
    configuration: {
      blockPublicAcls: true,
      ignorePublicAcls: true,
      blockPublicPolicy: true,
      restrictPublicBuckets: true,
    },
  });

  await store.createBucket({ bucket: postDemo.bucket });

  await store.putBucketCors({
    bucket: postDemo.bucket,
    corsRules: [
      {
        allowedOrigins: [appOrigin],
        allowedMethods: ["POST", "GET", "HEAD"],
        allowedHeaders: ["content-type"],
        exposeHeaders: ["ETag", "Location"],
      },
    ],
  });

  await store.putPublicAccessBlock({
    bucket: postDemo.bucket,
    configuration: {
      blockPublicAcls: true,
      ignorePublicAcls: true,
      blockPublicPolicy: true,
      restrictPublicBuckets: true,
    },
  });

  const uploader = await store.createPrincipal({
    name: "attachment-uploader",
  });

  const backend = await store.createPrincipal({ name: "attachment-backend" });
  const resource = `arn:aws:s3:::${bucket}/${prefix}*`;

  await store.putPrincipalPolicy({
    principalId: uploader.principalId,
    document: JSON.stringify({
      Version: "2012-10-17",
      Statement: {
        Effect: "Allow",
        Action: "s3:PutObject",
        Resource: resource,
        Condition: { StringEquals: { "s3:if-none-match": "*" } },
      },
    }),
  });

  await store.putPrincipalPolicy({
    principalId: backend.principalId,
    document: createBackendPolicy(bucket, prefix, false, postDemo),
  });

  const uploadKey = await store.createAccessKey({
    principalId: uploader.principalId,
  });

  const backendKey = await store.createAccessKey({
    principalId: backend.principalId,
  });

  const config: Config = {
    appOrigin,
    postDemo,
    appPort: 3000,
    state: {
      path: join(root, "app.sqlite3"),
      storageId: randomUUID(),
      bucket,
      prefix,
    },
    local: { directory: join(root, "store"), port: 3001 },
    storage: {
      endpoint: "http://127.0.0.1:3001",
      region: "us-east-1",
      uploader: {
        accessKeyId: uploadKey.accessKey.accessKeyId,
        secretAccessKey: uploadKey.secretAccessKey,
      },
      backend: {
        accessKeyId: backendKey.accessKey.accessKeyId,
        secretAccessKey: backendKey.secretAccessKey,
      },
    },
  };

  const state = await State.open({ ...config.state, initialize: true });
  await state.close();
  const path = join(root, "config.json");

  await writeFile(path, JSON.stringify(config, null, 2) + "\n", {
    flag: "wx",
    mode: 0o600,
  });

  return path;
}

/** Replace the app-owned backend policy after checking its configured identity. */
export async function configureLocalReconciliation(
  config: Config,
  principalId: string,
): Promise<void> {
  if (!config.local) {
    throw new AppError("invalid");
  }

  await using store = await Store.open({ directory: config.local.directory });
  await grantBackend(store, config, principalId);
}

async function grantBackend(
  store: Store,
  config: Config,
  principalId: string,
): Promise<void> {
  let continuationToken: string | undefined;

  for (;;) {
    const page = await store.listAccessKeys({
      principalId,
      continuationToken,
      maxResults: 100,
    });

    const ownsBackendKey = page.accessKeys.some(
      (key) => key.accessKeyId === config.storage.backend.accessKeyId,
    );

    if (ownsBackendKey) {
      break;
    }

    continuationToken = page.nextContinuationToken;

    if (continuationToken === undefined) {
      throw new AppError("invalid");
    }
  }

  await store.putPrincipalPolicy({
    principalId,
    document: createBackendPolicy(
      config.state.bucket,
      config.state.prefix,
      config.publicAttachments === true,
      config.postDemo,
    ),
  });
}

/** Explicit opt-in for the dedicated example bucket, after checking its backend. */
export async function configureLocalPublication(
  config: Config,
  principalId: string,
): Promise<void> {
  if (!config.local) {
    throw new AppError("invalid");
  }

  await using store = await Store.open({ directory: config.local.directory });
  const bucket = config.state.bucket;

  const document = JSON.stringify({
    Version: "2012-10-17",
    Statement: {
      Effect: "Allow",
      Principal: "*",
      Action: "s3:GetObject",
      Resource: `arn:aws:s3:::${bucket}/${config.state.prefix}*`,
      Condition: {
        StringEquals: { "s3:ExistingObjectTag/visibility": "public" },
      },
    },
  });

  try {
    const current = await store.getBucketPolicy({ bucket });

    if (current.document !== document) {
      throw new AppError("conflict");
    }
  } catch (error) {
    const isMissingPolicy =
      error instanceof StorageError && error.code === "NoSuchBucketPolicy";

    if (!isMissingPolicy) {
      throw error;
    }
  }

  await grantBackend(
    store,
    { ...config, publicAttachments: true },
    principalId,
  );

  // The command is deliberately separate from initialization. It replaces this
  // example's bucket policy and must only target its dedicated owned bucket.
  await store.putPublicAccessBlock({
    bucket,
    configuration: {
      blockPublicAcls: true,
      ignorePublicAcls: true,
      blockPublicPolicy: false,
      restrictPublicBuckets: false,
    },
  });

  await store.putBucketPolicy({ bucket, document });
}

/** Build the app backend's policy for its exclusive attachment and demo prefixes. */
export function createBackendPolicy(
  bucket: string,
  prefix: string,
  publication = false,
  postDemo?: { bucket: string; prefix: string },
): string {
  return JSON.stringify({
    Version: "2012-10-17",
    Statement: [
      ...(postDemo
        ? [
            {
              Effect: "Allow",
              Action: ["s3:PutObject", "s3:GetObject", "s3:DeleteObject"],
              Resource: `arn:aws:s3:::${postDemo.bucket}/${postDemo.prefix}*`,
            },
            {
              Effect: "Allow",
              Action: "s3:ListBucket",
              Resource: `arn:aws:s3:::${postDemo.bucket}`,
              Condition: { StringEquals: { "s3:prefix": postDemo.prefix } },
            },
          ]
        : []),
      {
        Effect: "Allow",
        Action: [
          "s3:GetObject",
          "s3:DeleteObject",
          ...(publication
            ? ["s3:PutObjectTagging", "s3:GetObjectTagging"]
            : []),
        ],
        Resource: `arn:aws:s3:::${bucket}/${prefix}*`,
      },
      {
        Effect: "Allow",
        Action: "s3:ListBucket",
        Resource: `arn:aws:s3:::${bucket}`,
        Condition: { StringEquals: { "s3:prefix": prefix } },
      },
    ],
  });
}
