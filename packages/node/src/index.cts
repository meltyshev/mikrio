import { markAsUncloneable } from "node:worker_threads";
import * as attributes from "./object-attributes.cjs";
import * as multipart from "./multipart.cjs";
import type * as Multipart from "./multipart-values.cjs";
import {
  principalInfo,
  accessKeyInfo,
  CreatedAccessKey,
  StoredPolicy,
} from "./admin.cjs";
import { corsRules, corsOutput } from "./cors.cjs";
import { lifecycleRules, lifecycleOutput } from "./lifecycle.cjs";
import { publicAccessBlock, aclOutput } from "./access-configuration.cjs";
import type {
  GetObjectAttributesInput,
  GetObjectAttributesOutput,
  GetObjectTaggingInput,
  GetObjectTaggingOutput,
  PutObjectTaggingInput,
  PutObjectTaggingOutput,
  DeleteObjectTaggingInput,
  DeleteObjectTaggingOutput,
  PutBucketCorsInput,
  PutBucketLifecycleConfigurationInput,
  PutBucketCorsOutput,
  PutBucketLifecycleConfigurationOutput,
  GetBucketCorsInput,
  GetBucketLifecycleConfigurationInput,
  GetBucketCorsOutput,
  GetBucketLifecycleConfigurationOutput,
  DeleteBucketCorsInput,
  DeleteBucketLifecycleInput,
  DeleteBucketCorsOutput,
  DeleteBucketLifecycleOutput,
  PutBucketPolicyInput,
  PutBucketPolicyOutput,
  GetBucketPolicyInput,
  GetBucketPolicyOutput,
  DeleteBucketPolicyInput,
  DeleteBucketPolicyOutput,
  GetBucketPolicyStatusInput,
  GetBucketPolicyStatusOutput,
  PutPublicAccessBlockInput,
  PutPublicAccessBlockOutput,
  GetPublicAccessBlockInput,
  GetPublicAccessBlockOutput,
  DeletePublicAccessBlockInput,
  DeletePublicAccessBlockOutput,
  PutBucketOwnershipControlsInput,
  PutBucketOwnershipControlsOutput,
  GetBucketOwnershipControlsInput,
  GetBucketOwnershipControlsOutput,
  GetBucketAclInput,
  GetBucketAclOutput,
  GetObjectAclInput,
  GetObjectAclOutput,
} from "./values.cjs";
import { listen, type ListenOptions, type Server } from "./listener.cjs";
import type {
  CreateAccessKeyInput,
  CreateAccessKeyOutput,
  CreatePrincipalInput,
  DeleteAccessKeyInput,
  DeleteAccessKeyOutput,
  DeletePrincipalInput,
  DeletePrincipalOutput,
  DeletePrincipalPolicyInput,
  DeletePrincipalPolicyOutput,
  GetPrincipalInput,
  GetPrincipalPolicyInput,
  GetPrincipalPolicyOutput,
  ListAccessKeysInput,
  ListAccessKeysOutput,
  ListPrincipalsInput,
  ListPrincipalsOutput,
  OwnerInfo,
  PrincipalInfo,
  PutPrincipalPolicyInput,
  PutPrincipalPolicyOutput,
  SetPrincipalEnabledInput,
} from "./values.cjs";
import { startDelivery } from "./diagnostics.cjs";
import {
  StorageError,
  beforeMutation,
  canceled,
  storageError,
} from "./errors.cjs";
import { loadNative, type NativeStore } from "./native.cjs";
import { Call, callSignal, invoke } from "./calls.cjs";
import {
  tags,
  bucketInput,
  listObjectsInput,
  listBucketsInput,
  deleteInput,
  deleteObjectsInput,
  putInput,
  readInput,
  readOutput,
  record,
  string,
} from "./inputs.cjs";
import { fromNative } from "./body.cjs";
import { put } from "./upload.cjs";
import type {
  CreateBucketInput,
  ListObjectsV2Input,
  ListBucketsInput,
  ListObjectsV2Output,
  ListBucketsOutput,
  CreateBucketOutput,
  HeadBucketInput,
  HeadBucketOutput,
  GetBucketLocationInput,
  GetBucketLocationOutput,
  DeleteBucketInput,
  DeleteBucketOutput,
  PutObjectInput,
  PutObjectOutput,
  GetObjectInput,
  GetObjectOutput,
  HeadObjectInput,
  HeadObjectOutput,
  DeleteObjectInput,
  DeleteObjectOutput,
  DeleteObjectsInput,
  DeleteObjectsOutput,
} from "./values.cjs";

export { StorageError } from "./errors.cjs";
export { ObjectBody } from "./body.cjs";
export { Server } from "./listener.cjs";
export type { Credentials, ListenOptions } from "./listener.cjs";
export type * from "./values.cjs";

/** Values for offline local initialization or reopen. */
export interface OpenOptions {
  directory: string;
  region?: string | undefined;
  /** Defaults to true; false requires recognized storage and still permits recovery. */
  createIfMissing?: boolean | undefined;
}

/** Per-call cancellation. GET retains its signal until body EOF or close. */
export interface CallOptions {
  signal?: AbortSignal | undefined;
}

/** Detached facts remain readable after close. */
export interface StoreInfo {
  readonly storeId: string;
  readonly formatVersion: number;
  readonly directory: string;
  readonly region: string;
}

const CONSTRUCTOR_KEY = Symbol("Store constructor");

/**
 * One independently opened native connection. Close it explicitly to settle its
 * operations, bodies and listeners; other connections remain usable.
 * Methods reject with TypeError for host misuse or StorageError for domain failures.
 * Cancellation does not establish rollback; inspect mutationOutcome on failures.
 */
export class Store {
  readonly #native: NativeStore;
  readonly #info: StoreInfo;
  #closing: Promise<void> | undefined;

  private constructor(native: NativeStore, key: typeof CONSTRUCTOR_KEY) {
    if (key !== CONSTRUCTOR_KEY) {
      throw new TypeError("Use Store.open to acquire a Store");
    }

    this.#native = native;
    this.#info = Object.freeze(native.info);
    markAsUncloneable(this);
    startDelivery(native.diagnostics);
  }

  /** Create recognized storage or reopen it; never reset a directory. */
  static async open(
    options: OpenOptions,
    callOptions?: CallOptions,
  ): Promise<Store> {
    const { directory, region, createIfMissing } = openOptions(options);
    const signal = callSignal(callOptions);

    if (signal?.aborted) {
      throw canceled(signal);
    }

    const native = loadNative();
    const cancellation = new native.NativeCancellation();
    const onAbort = (): void => cancellation.cancel();
    signal?.addEventListener("abort", onAbort, { once: true });

    try {
      // Recheck after registration so a signal cannot be missed during setup.
      if (signal?.aborted) {
        throw canceled(signal);
      }

      const store = await native.open(
        directory,
        region,
        createIfMissing ?? true,
        cancellation,
      );

      if (signal?.aborted) {
        const error = canceled(signal);

        try {
          await store.close();
        } catch (closeError: unknown) {
          throw new AggregateError(
            [error, storageError(closeError)],
            "Open cancellation cleanup failed",
            { cause: closeError },
          );
        }

        throw error;
      }

      return new Store(store, CONSTRUCTOR_KEY);
    } catch (error: unknown) {
      const translated = storageError(error);

      const wasCanceled =
        translated instanceof StorageError &&
        translated.code === "OperationCanceled";

      if (wasCanceled && signal?.aborted) {
        throw canceled(signal);
      }

      throw translated;
    } finally {
      signal?.removeEventListener("abort", onAbort);
    }
  }

  get info(): StoreInfo {
    return this.#info;
  }

  /** Bind one ready listener on this connection's engine. */
  async listen(
    options?: ListenOptions,
    callOptions?: CallOptions,
  ): Promise<Server> {
    return await listen(this.#native, options, callOptions);
  }

  /** Trusted local getOwnerInfo; results are detached from storage. */
  async getOwnerInfo(options?: CallOptions): Promise<OwnerInfo> {
    const output = await invoke(options, false, (cancellation) =>
      this.#native.getOwnerInfo(cancellation),
    );

    return Object.freeze(output);
  }

  /** Trusted local createPrincipal; mutation errors retain their outcome. */
  async createPrincipal(
    input: CreatePrincipalInput,
    options?: CallOptions,
  ): Promise<PrincipalInfo> {
    try {
      const fields = record(input, ["name"], "CreatePrincipalInput");

      const request = {
        name: string(fields.name),
      };

      const output = await invoke(options, true, (cancellation) =>
        this.#native.createPrincipal(request, cancellation),
      );

      return principalInfo(output);
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Trusted local getPrincipal; results are detached from storage. */
  async getPrincipal(
    input: GetPrincipalInput,
    options?: CallOptions,
  ): Promise<PrincipalInfo> {
    const fields = record(input, ["principalId"], "GetPrincipalInput");

    const request = {
      principalId: string(fields.principalId),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getPrincipal(request, cancellation),
    );

    return principalInfo(output);
  }

  /** Trusted local setPrincipalEnabled; mutation errors retain their outcome. */
  async setPrincipalEnabled(
    input: SetPrincipalEnabledInput,
    options?: CallOptions,
  ): Promise<PrincipalInfo> {
    try {
      const fields = record(
        input,
        ["principalId", "isEnabled"],
        "SetPrincipalEnabledInput",
      );

      if (typeof fields.isEnabled !== "boolean") {
        throw new TypeError("isEnabled must be boolean");
      }

      const request = {
        principalId: string(fields.principalId),
        isEnabled: fields.isEnabled,
      };

      const output = await invoke(options, true, (cancellation) =>
        this.#native.setPrincipalEnabled(request, cancellation),
      );

      return principalInfo(output);
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Trusted local deletePrincipal; mutation errors retain their outcome. */
  async deletePrincipal(
    input: DeletePrincipalInput,
    options?: CallOptions,
  ): Promise<DeletePrincipalOutput> {
    try {
      const fields = record(input, ["principalId"], "DeletePrincipalInput");

      const request = {
        principalId: string(fields.principalId),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.deletePrincipal(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Trusted local putPrincipalPolicy; mutation errors retain their outcome. */
  async putPrincipalPolicy(
    input: PutPrincipalPolicyInput,
    options?: CallOptions,
  ): Promise<PutPrincipalPolicyOutput> {
    try {
      const fields = record(
        input,
        ["principalId", "document"],
        "PutPrincipalPolicyInput",
      );

      const request = {
        principalId: string(fields.principalId),
        document: string(fields.document),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.putPrincipalPolicy(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Trusted local getPrincipalPolicy; results are detached from storage. */
  async getPrincipalPolicy(
    input: GetPrincipalPolicyInput,
    options?: CallOptions,
  ): Promise<GetPrincipalPolicyOutput> {
    const fields = record(input, ["principalId"], "GetPrincipalPolicyInput");

    const request = {
      principalId: string(fields.principalId),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getPrincipalPolicy(request, cancellation),
    );

    return new StoredPolicy(output.document);
  }

  /** Trusted local deletePrincipalPolicy; mutation errors retain their outcome. */
  async deletePrincipalPolicy(
    input: DeletePrincipalPolicyInput,
    options?: CallOptions,
  ): Promise<DeletePrincipalPolicyOutput> {
    try {
      const fields = record(
        input,
        ["principalId"],
        "DeletePrincipalPolicyInput",
      );

      const request = {
        principalId: string(fields.principalId),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.deletePrincipalPolicy(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Trusted local createAccessKey; mutation errors retain their outcome. */
  async createAccessKey(
    input: CreateAccessKeyInput,
    options?: CallOptions,
  ): Promise<CreateAccessKeyOutput> {
    try {
      const fields = record(input, ["principalId"], "CreateAccessKeyInput");

      const request = {
        principalId: string(fields.principalId),
      };

      const output = await invoke(options, true, (cancellation) =>
        this.#native.createAccessKey(request, cancellation),
      );

      return new CreatedAccessKey(output.accessKey, output.secretAccessKey);
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Trusted local deleteAccessKey; mutation errors retain their outcome. */
  async deleteAccessKey(
    input: DeleteAccessKeyInput,
    options?: CallOptions,
  ): Promise<DeleteAccessKeyOutput> {
    try {
      const fields = record(
        input,
        ["principalId", "accessKeyId"],
        "DeleteAccessKeyInput",
      );

      const request = {
        principalId: string(fields.principalId),
        accessKeyId: string(fields.accessKeyId),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.deleteAccessKey(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Trusted local listPrincipals; results are detached from storage. */
  async listPrincipals(
    input: ListPrincipalsInput,
    options?: CallOptions,
  ): Promise<ListPrincipalsOutput> {
    const fields = record(
      input,
      ["maxResults", "continuationToken"],
      "ListPrincipalsInput",
    );

    if (fields.maxResults !== undefined) {
      if (typeof fields.maxResults !== "number") {
        throw new TypeError("maxResults must be a number");
      }
    }

    const request = {
      maxResults: fields.maxResults,
      continuationToken:
        fields.continuationToken === undefined
          ? undefined
          : string(fields.continuationToken),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.listPrincipals(request, cancellation),
    );

    return Object.freeze({
      principals: Object.freeze(output.principals.map(principalInfo)),
      nextContinuationToken: output.nextContinuationToken,
    });
  }

  /** Trusted local listAccessKeys; results are detached from storage. */
  async listAccessKeys(
    input: ListAccessKeysInput,
    options?: CallOptions,
  ): Promise<ListAccessKeysOutput> {
    const fields = record(
      input,
      ["principalId", "maxResults", "continuationToken"],
      "ListAccessKeysInput",
    );

    if (fields.maxResults !== undefined) {
      if (typeof fields.maxResults !== "number") {
        throw new TypeError("maxResults must be a number");
      }
    }

    const request = {
      principalId: string(fields.principalId),
      maxResults: fields.maxResults,
      continuationToken:
        fields.continuationToken === undefined
          ? undefined
          : string(fields.continuationToken),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.listAccessKeys(request, cancellation),
    );

    return Object.freeze({
      accessKeys: Object.freeze(output.accessKeys.map(accessKeyInfo)),
      nextContinuationToken: output.nextContinuationToken,
    });
  }

  /** Read selected detached Attributes and a bounded completed-part page. */
  async getObjectAttributes(
    input: GetObjectAttributesInput,
    options?: CallOptions,
  ): Promise<GetObjectAttributesOutput> {
    return attributes.get(this.#native, input, options);
  }

  /** Read detached tags. */
  async getObjectTagging(
    input: GetObjectTaggingInput,
    options?: CallOptions,
  ): Promise<GetObjectTaggingOutput> {
    const fields = record(
      input,
      ["bucket", "key", "expectedBucketOwner"],
      "GetObjectTaggingInput",
    );

    const request = {
      bucket: string(fields.bucket),
      key: string(fields.key),
      expectedBucketOwner:
        fields.expectedBucketOwner === undefined
          ? undefined
          : string(fields.expectedBucketOwner),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getObjectTagging(request, cancellation),
    );

    return Object.freeze({
      tags: Object.freeze(
        output.tags.map((tag) =>
          Object.freeze({ key: tag.key, value: tag.value }),
        ),
      ),
    });
  }

  /** Change the whole tag set without changing payload identity or age. */
  async putObjectTagging(
    input: PutObjectTaggingInput,
    options?: CallOptions,
  ): Promise<PutObjectTaggingOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "key", "expectedBucketOwner", "tags"],
        "PutObjectTaggingInput",
      );

      const requestedTags = tags(fields.tags);

      if (requestedTags === undefined) {
        throw new TypeError("tags is required");
      }

      const request = {
        bucket: string(fields.bucket),
        key: string(fields.key),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
        tags: requestedTags,
      };

      await invoke(options, true, (cancellation) =>
        this.#native.putObjectTagging(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Clear the tag set without changing payload identity or age. */
  async deleteObjectTagging(
    input: DeleteObjectTaggingInput,
    options?: CallOptions,
  ): Promise<DeleteObjectTaggingOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "key", "expectedBucketOwner"],
        "DeleteObjectTaggingInput",
      );

      const request = {
        bucket: string(fields.bucket),
        key: string(fields.key),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.deleteObjectTagging(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Replace the complete bucket CORS configuration. */
  async putBucketCors(
    input: PutBucketCorsInput,
    options?: CallOptions,
  ): Promise<PutBucketCorsOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "expectedBucketOwner", "corsRules"],
        "PutBucketCorsInput",
      );

      const request = {
        bucket: string(fields.bucket),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
        corsRules: corsRules(fields.corsRules),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.putBucketCors(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Read the exact stored bucket CORS configuration. */
  async getBucketCors(
    input: GetBucketCorsInput,
    options?: CallOptions,
  ): Promise<GetBucketCorsOutput> {
    const fields = record(
      input,
      ["bucket", "expectedBucketOwner"],
      "GetBucketCorsInput",
    );

    const request = {
      bucket: string(fields.bucket),
      expectedBucketOwner:
        fields.expectedBucketOwner === undefined
          ? undefined
          : string(fields.expectedBucketOwner),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getBucketCors(request, cancellation),
    );

    return corsOutput(output);
  }

  /** Delete the bucket CORS configuration idempotently. */
  async deleteBucketCors(
    input: DeleteBucketCorsInput,
    options?: CallOptions,
  ): Promise<DeleteBucketCorsOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "expectedBucketOwner"],
        "DeleteBucketCorsInput",
      );

      const request = {
        bucket: string(fields.bucket),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.deleteBucketCors(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Replace the complete bucket lifecycle configuration. */
  async putBucketLifecycleConfiguration(
    input: PutBucketLifecycleConfigurationInput,
    options?: CallOptions,
  ): Promise<PutBucketLifecycleConfigurationOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "expectedBucketOwner", "lifecycleRules"],
        "PutBucketLifecycleConfigurationInput",
      );

      const request = {
        bucket: string(fields.bucket),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
        lifecycleRules: lifecycleRules(fields.lifecycleRules),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.putBucketLifecycleConfiguration(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Read the exact stored bucket lifecycle configuration. */
  async getBucketLifecycleConfiguration(
    input: GetBucketLifecycleConfigurationInput,
    options?: CallOptions,
  ): Promise<GetBucketLifecycleConfigurationOutput> {
    const fields = record(
      input,
      ["bucket", "expectedBucketOwner"],
      "GetBucketLifecycleConfigurationInput",
    );

    const request = {
      bucket: string(fields.bucket),
      expectedBucketOwner:
        fields.expectedBucketOwner === undefined
          ? undefined
          : string(fields.expectedBucketOwner),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getBucketLifecycleConfiguration(request, cancellation),
    );

    return lifecycleOutput(output);
  }

  /** Delete the bucket lifecycle configuration idempotently. */
  async deleteBucketLifecycle(
    input: DeleteBucketLifecycleInput,
    options?: CallOptions,
  ): Promise<DeleteBucketLifecycleOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "expectedBucketOwner"],
        "DeleteBucketLifecycleInput",
      );

      const request = {
        bucket: string(fields.bucket),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.deleteBucketLifecycle(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Replace the complete bucket policy. */
  async putBucketPolicy(
    input: PutBucketPolicyInput,
    options?: CallOptions,
  ): Promise<PutBucketPolicyOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "expectedBucketOwner", "document"],
        "PutBucketPolicyInput",
      );

      const request = {
        bucket: string(fields.bucket),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
        document: string(fields.document),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.putBucketPolicy(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Read the exact stored bucket policy. */
  async getBucketPolicy(
    input: GetBucketPolicyInput,
    options?: CallOptions,
  ): Promise<GetBucketPolicyOutput> {
    const fields = record(
      input,
      ["bucket", "expectedBucketOwner"],
      "GetBucketPolicyInput",
    );

    const request = {
      bucket: string(fields.bucket),
      expectedBucketOwner:
        fields.expectedBucketOwner === undefined
          ? undefined
          : string(fields.expectedBucketOwner),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getBucketPolicy(request, cancellation),
    );

    return new StoredPolicy(output.document);
  }

  /** Delete the bucket policy idempotently. */
  async deleteBucketPolicy(
    input: DeleteBucketPolicyInput,
    options?: CallOptions,
  ): Promise<DeleteBucketPolicyOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "expectedBucketOwner"],
        "DeleteBucketPolicyInput",
      );

      const request = {
        bucket: string(fields.bucket),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.deleteBucketPolicy(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Read public-policy classification independently of access enforcement. */
  async getBucketPolicyStatus(
    input: GetBucketPolicyStatusInput,
    options?: CallOptions,
  ): Promise<GetBucketPolicyStatusOutput> {
    const fields = record(
      input,
      ["bucket", "expectedBucketOwner"],
      "GetBucketPolicyStatusInput",
    );

    const request = {
      bucket: string(fields.bucket),
      expectedBucketOwner:
        fields.expectedBucketOwner === undefined
          ? undefined
          : string(fields.expectedBucketOwner),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getBucketPolicyStatus(request, cancellation),
    );

    return Object.freeze(output);
  }

  /** Replace all four public-access-block flags. */
  async putPublicAccessBlock(
    input: PutPublicAccessBlockInput,
    options?: CallOptions,
  ): Promise<PutPublicAccessBlockOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "expectedBucketOwner", "configuration"],
        "PutPublicAccessBlockInput",
      );

      const request = {
        bucket: string(fields.bucket),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
        configuration: publicAccessBlock(fields.configuration),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.putPublicAccessBlock(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Read the present public-access-block configuration. */
  async getPublicAccessBlock(
    input: GetPublicAccessBlockInput,
    options?: CallOptions,
  ): Promise<GetPublicAccessBlockOutput> {
    const fields = record(
      input,
      ["bucket", "expectedBucketOwner"],
      "GetPublicAccessBlockInput",
    );

    const request = {
      bucket: string(fields.bucket),
      expectedBucketOwner:
        fields.expectedBucketOwner === undefined
          ? undefined
          : string(fields.expectedBucketOwner),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getPublicAccessBlock(request, cancellation),
    );

    return Object.freeze({
      configuration: Object.freeze(output.configuration),
    });
  }

  /** Delete the public-access-block configuration idempotently. */
  async deletePublicAccessBlock(
    input: DeletePublicAccessBlockInput,
    options?: CallOptions,
  ): Promise<DeletePublicAccessBlockOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "expectedBucketOwner"],
        "DeletePublicAccessBlockInput",
      );

      const request = {
        bucket: string(fields.bucket),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
      };

      await invoke(options, true, (cancellation) =>
        this.#native.deletePublicAccessBlock(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Assert the fixed bucket-owner-enforced ownership model. */
  async putBucketOwnershipControls(
    input: PutBucketOwnershipControlsInput,
    options?: CallOptions,
  ): Promise<PutBucketOwnershipControlsOutput> {
    try {
      const fields = record(
        input,
        ["bucket", "expectedBucketOwner", "objectOwnership"],
        "PutBucketOwnershipControlsInput",
      );

      const ownership = string(fields.objectOwnership);

      if (ownership !== "BucketOwnerEnforced") {
        throw new StorageError("InvalidArgument", "unsupported ownership mode");
      }

      const request: PutBucketOwnershipControlsInput = {
        bucket: string(fields.bucket),
        expectedBucketOwner:
          fields.expectedBucketOwner === undefined
            ? undefined
            : string(fields.expectedBucketOwner),
        objectOwnership: ownership,
      };

      await invoke(options, true, (cancellation) =>
        this.#native.putBucketOwnershipControls(request, cancellation),
      );

      return Object.freeze({});
    } catch (error: unknown) {
      throw beforeMutation(error);
    }
  }

  /** Read the fixed bucket-owner-enforced ownership model. */
  async getBucketOwnershipControls(
    input: GetBucketOwnershipControlsInput,
    options?: CallOptions,
  ): Promise<GetBucketOwnershipControlsOutput> {
    const fields = record(
      input,
      ["bucket", "expectedBucketOwner"],
      "GetBucketOwnershipControlsInput",
    );

    const request = {
      bucket: string(fields.bucket),
      expectedBucketOwner:
        fields.expectedBucketOwner === undefined
          ? undefined
          : string(fields.expectedBucketOwner),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getBucketOwnershipControls(request, cancellation),
    );

    return Object.freeze(output);
  }

  /** Read the canonical owner and its full-control grant. */
  async getBucketAcl(
    input: GetBucketAclInput,
    options?: CallOptions,
  ): Promise<GetBucketAclOutput> {
    const fields = record(
      input,
      ["bucket", "expectedBucketOwner"],
      "GetBucketAclInput",
    );

    const request = {
      bucket: string(fields.bucket),
      expectedBucketOwner:
        fields.expectedBucketOwner === undefined
          ? undefined
          : string(fields.expectedBucketOwner),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getBucketAcl(request, cancellation),
    );

    return aclOutput(output);
  }

  /** Read the canonical object ACL. */
  async getObjectAcl(
    input: GetObjectAclInput,
    options?: CallOptions,
  ): Promise<GetObjectAclOutput> {
    const fields = record(
      input,
      ["bucket", "key", "expectedBucketOwner"],
      "GetObjectAclInput",
    );

    const request = {
      bucket: string(fields.bucket),
      key: string(fields.key),
      expectedBucketOwner:
        fields.expectedBucketOwner === undefined
          ? undefined
          : string(fields.expectedBucketOwner),
    };

    const output = await invoke(options, false, (cancellation) =>
      this.#native.getObjectAcl(request, cancellation),
    );

    return aclOutput(output);
  }

  /** Read one live bucket page, preserving the exact prefix across continuation. */
  async listBuckets(
    input: ListBucketsInput = {},
    options?: CallOptions,
  ): Promise<ListBucketsOutput> {
    const request = listBucketsInput(input);

    const output = await invoke(options, false, (cancellation) =>
      this.#native.listBuckets(request, cancellation),
    );

    return Object.freeze({
      buckets: Object.freeze(
        output.buckets.map((bucket) =>
          Object.freeze({
            name: bucket.name,
            creationDate: new Date(bucket.creationDate),
            bucketRegion: bucket.bucketRegion,
          }),
        ),
      ),
      continuationToken: output.continuationToken ?? undefined,
    });
  }

  /** Read one consistent page; continuation uses the same prefix and delimiter. */
  async listObjectsV2(
    input: ListObjectsV2Input,
    options?: CallOptions,
  ): Promise<ListObjectsV2Output> {
    const request = listObjectsInput(input);

    const output = await invoke(options, false, (cancellation) =>
      this.#native.listObjectsV2(request, cancellation),
    );

    return Object.freeze({
      contents: Object.freeze(
        output.contents.map((object) =>
          Object.freeze({
            key: object.key,
            size: object.size,
            etag: object.etag,
            lastModified: new Date(object.lastModified),
          }),
        ),
      ),
      commonPrefixes: Object.freeze(
        output.commonPrefixes.map((prefix) => Object.freeze({ prefix })),
      ),
      isTruncated: output.isTruncated,
      nextContinuationToken: output.nextContinuationToken ?? undefined,
    });
  }

  /** Create a bucket using the store's persisted region. */
  async createBucket(
    input: CreateBucketInput,
    options?: CallOptions,
  ): Promise<CreateBucketOutput> {
    const fields = record(input, ["bucket"], "CreateBucketInput");
    const bucket = string(fields.bucket);

    await invoke(options, true, (cancellation) =>
      this.#native.createBucket(bucket, cancellation),
    );

    return Object.freeze({});
  }

  /** Check the current bucket and optional owner assertion. */
  async headBucket(
    input: HeadBucketInput,
    options?: CallOptions,
  ): Promise<HeadBucketOutput> {
    const fields = bucketInput(input);

    const bucketRegion = await invoke(options, false, (cancellation) =>
      this.#native.headBucket(fields, cancellation),
    );

    return Object.freeze({ bucketRegion });
  }

  /** Return undefined for the us-east-1 location constraint. */
  async getBucketLocation(
    input: GetBucketLocationInput,
    options?: CallOptions,
  ): Promise<GetBucketLocationOutput> {
    const fields = bucketInput(input);

    const location = await invoke(options, false, (cancellation) =>
      this.#native.getBucketLocation(fields, cancellation),
    );

    return Object.freeze({ locationConstraint: location ?? undefined });
  }

  /** Retire a bucket with no current objects; physical cleanup is automatic. */
  async deleteBucket(
    input: DeleteBucketInput,
    options?: CallOptions,
  ): Promise<DeleteBucketOutput> {
    const fields = bucketInput(input);

    await invoke(options, true, (cancellation) =>
      this.#native.deleteBucket(fields, cancellation),
    );

    return Object.freeze({});
  }

  /** Copy a selected source object under optional source and destination conditions. */
  async copyObject(
    input: Multipart.CopyObjectInput,
    options?: CallOptions,
  ): Promise<Multipart.CopyObjectOutput> {
    return await multipart.copyObject(this.#native, input, options);
  }

  /** Create a durable upload that survives connection close until completion or abort. */
  async createMultipartUpload(
    input: Multipart.CreateMultipartUploadInput,
    options?: CallOptions,
  ): Promise<Multipart.CreateMultipartUploadOutput> {
    return await multipart.createMultipartUpload(this.#native, input, options);
  }

  /** Stream one part with backpressure; producer failures preserve their original cause. */
  async uploadPart(
    input: Multipart.UploadPartInput,
    options?: CallOptions,
  ): Promise<Multipart.UploadPartOutput> {
    return await multipart.uploadPart(this.#native, input, options);
  }

  /** Copy a selected source object or range into one durable upload part. */
  async uploadPartCopy(
    input: Multipart.UploadPartCopyInput,
    options?: CallOptions,
  ): Promise<Multipart.UploadPartCopyOutput> {
    return await multipart.uploadPartCopy(this.#native, input, options);
  }

  /** Publish the selected parts atomically; mutation errors retain their outcome. */
  async completeMultipartUpload(
    input: Multipart.CompleteMultipartUploadInput,
    options?: CallOptions,
  ): Promise<Multipart.CompleteMultipartUploadOutput> {
    return await multipart.completeMultipartUpload(
      this.#native,
      input,
      options,
    );
  }

  /** Retire an incomplete upload and schedule its parts for cleanup. */
  async abortMultipartUpload(
    input: Multipart.AbortMultipartUploadInput,
    options?: CallOptions,
  ): Promise<Multipart.AbortMultipartUploadOutput> {
    return await multipart.abortMultipartUpload(this.#native, input, options);
  }

  /** Read a bounded page of detached part facts for an incomplete upload. */
  async listParts(
    input: Multipart.ListPartsInput,
    options?: CallOptions,
  ): Promise<Multipart.ListPartsOutput> {
    return await multipart.listParts(this.#native, input, options);
  }

  /** Read a bounded page of incomplete uploads and literal common prefixes. */
  async listMultipartUploads(
    input: Multipart.ListMultipartUploadsInput,
    options?: CallOptions,
  ): Promise<Multipart.ListMultipartUploadsOutput> {
    return await multipart.listMultipartUploads(this.#native, input, options);
  }

  /** Pull with backpressure and publish verified bytes atomically, without replay. */
  async putObject(
    input: PutObjectInput,
    options?: CallOptions,
  ): Promise<PutObjectOutput> {
    let fields: ReturnType<typeof putInput>;

    try {
      fields = putInput(input);
    } catch (error: unknown) {
      throw beforeMutation(error);
    }

    return await put(this.#native, fields.input, fields.body, options);
  }

  /** Return coherent metadata and one protected cursor; close unused bodies. */
  async getObject(
    input: GetObjectInput,
    options?: CallOptions,
  ): Promise<GetObjectOutput> {
    const fields = readInput(input);
    const call = new Call(options, false);
    let selected: Awaited<ReturnType<NativeStore["getObject"]>> | undefined;

    try {
      selected = await this.#native.getObject(fields, call.cancellation);
      const facts = readOutput(selected.facts);
      const body = await fromNative(selected.body, call);

      return Object.freeze({ ...facts, body });
    } catch (error: unknown) {
      call.close();
      const converted = storageError(error);

      const primary =
        converted instanceof StorageError &&
        converted.code === "OperationCanceled" &&
        call.signal?.aborted
          ? canceled(call.signal)
          : converted;

      if (selected !== undefined) {
        try {
          await selected.body.close();
        } catch (cleanup: unknown) {
          throw new AggregateError(
            [primary, storageError(cleanup)],
            "GET cleanup failed",
            { cause: cleanup },
          );
        }
      }

      throw primary;
    }
  }

  /** Read current metadata and conditions without opening the payload. */
  async headObject(
    input: HeadObjectInput,
    options?: CallOptions,
  ): Promise<HeadObjectOutput> {
    const fields = readInput(input);

    const output = await invoke(options, false, (cancellation) =>
      this.#native.headObject(fields, cancellation),
    );

    return readOutput(output);
  }

  /** Delete the current mapping atomically under an optional ETag condition. */
  async deleteObject(
    input: DeleteObjectInput,
    options?: CallOptions,
  ): Promise<DeleteObjectOutput> {
    const fields = deleteInput(input);

    await invoke(options, true, (cancellation) =>
      this.#native.deleteObject(fields, cancellation),
    );

    return Object.freeze({});
  }

  /** Delete independent entries in order, retaining indexed progress on interruption. */
  async deleteObjects(
    input: DeleteObjectsInput,
    options?: CallOptions,
  ): Promise<DeleteObjectsOutput> {
    let fields: DeleteObjectsInput;

    try {
      fields = deleteObjectsInput(input);
    } catch (error: unknown) {
      throw beforeMutation(error);
    }

    const output = await invoke(options, true, (cancellation) =>
      this.#native.deleteObjects(fields, cancellation),
    );

    return Object.freeze({
      deleted: Object.freeze(
        output.deleted.map((item) => Object.freeze({ ...item })),
      ),
      errors: Object.freeze(
        output.errors.map((item) => Object.freeze({ ...item })),
      ),
    });
  }

  /** Repeated/concurrent calls join native shutdown; failure permits retry. */
  async close(): Promise<void> {
    let attempt: Promise<void> | undefined;

    try {
      attempt = this.#closing ?? this.#native.close();
      this.#closing = attempt;
      await attempt;
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

function openOptions(value: unknown): OpenOptions {
  const input = record(
    value,
    ["directory", "region", "createIfMissing"],
    "OpenOptions",
  );

  if (typeof input.directory !== "string") {
    throw new TypeError("directory must be a string");
  }

  if (input.region !== undefined && typeof input.region !== "string") {
    throw new TypeError("region must be a string or undefined");
  }

  const createIfMissing = input.createIfMissing;

  if (createIfMissing !== undefined && typeof createIfMissing !== "boolean") {
    throw new TypeError("createIfMissing must be a boolean or undefined");
  }

  return {
    directory: input.directory,
    region: input.region,
    createIfMissing,
  };
}
