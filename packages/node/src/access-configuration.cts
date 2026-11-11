import { record } from "./inputs.cjs";
import type {
  PublicAccessBlockConfiguration,
  GetBucketAclOutput,
} from "./values.cjs";

function boolean(value: unknown): boolean {
  if (typeof value !== "boolean") {
    throw new TypeError("all public access block flags must be boolean");
  }

  return value;
}

/** Snapshot the caller's complete value before any asynchronous dispatch. */
export function publicAccessBlock(
  value: unknown,
): PublicAccessBlockConfiguration {
  const fields = record(
    value,
    [
      "blockPublicAcls",
      "ignorePublicAcls",
      "blockPublicPolicy",
      "restrictPublicBuckets",
    ],
    "PublicAccessBlockConfiguration",
  );

  return {
    blockPublicAcls: boolean(fields.blockPublicAcls),
    ignorePublicAcls: boolean(fields.ignorePublicAcls),
    blockPublicPolicy: boolean(fields.blockPublicPolicy),
    restrictPublicBuckets: boolean(fields.restrictPublicBuckets),
  };
}

/** Both ACL operations return the same detached canonical owner/grant shape. */
export function aclOutput(value: GetBucketAclOutput): GetBucketAclOutput {
  return Object.freeze({
    owner: Object.freeze(value.owner),
    grants: Object.freeze(value.grants.map((grant) => Object.freeze(grant))),
  });
}
