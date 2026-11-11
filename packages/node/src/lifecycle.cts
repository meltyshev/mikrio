import { StorageError } from "./errors.cjs";
import { optionalString, record, string } from "./inputs.cjs";
import type {
  LifecycleRule,
  GetBucketLifecycleConfigurationOutput,
} from "./values.cjs";

function days(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value)) {
    throw new TypeError("lifecycle days must be an integer");
  }

  return value;
}

/** Own and shape-check all nested inputs before asynchronous admission. */
export function lifecycleRules(value: unknown): LifecycleRule[] {
  if (!Array.isArray(value)) {
    throw new TypeError("lifecycleRules must be an array");
  }

  const length = value.length;

  if (length > 1000) {
    throw new StorageError(
      "InvalidArgument",
      "at most 1000 lifecycle rules are accepted",
    );
  }

  const rules: LifecycleRule[] = [];

  for (let index = 0; index < length; index++) {
    rules.push(lifecycleRule(value[index]));
  }

  return rules;
}

function lifecycleRule(rule: unknown): LifecycleRule {
  const fields = record(
    rule,
    ["id", "status", "filter", "expiration", "abortIncompleteMultipartUpload"],
    "LifecycleRule",
  );

  const {
    id,
    status: suppliedStatus,
    filter: suppliedFilter,
    expiration: suppliedExpiration,
    abortIncompleteMultipartUpload: suppliedAbort,
  } = fields;

  const status = string(suppliedStatus);

  if (status !== "Enabled" && status !== "Disabled") {
    throw new TypeError("invalid lifecycle status");
  }

  const filter = record(suppliedFilter, ["prefix"], "LifecycleFilter");

  const expiration =
    suppliedExpiration === undefined
      ? undefined
      : record(suppliedExpiration, ["days"], "LifecycleExpiration");

  const abort =
    suppliedAbort === undefined
      ? undefined
      : record(
          suppliedAbort,
          ["daysAfterInitiation"],
          "AbortIncompleteMultipartUpload",
        );

  return {
    id: optionalString(id),
    status,
    filter: {
      prefix: optionalString(filter.prefix),
    },
    expiration:
      expiration === undefined ? undefined : { days: days(expiration.days) },
    abortIncompleteMultipartUpload:
      abort === undefined
        ? undefined
        : { daysAfterInitiation: days(abort.daysAfterInitiation) },
  };
}

/** Detach and freeze nested values so callers cannot mutate an observed snapshot. */
export function lifecycleOutput(
  value: GetBucketLifecycleConfigurationOutput,
): GetBucketLifecycleConfigurationOutput {
  return Object.freeze({
    lifecycleRules: Object.freeze(
      value.lifecycleRules.map((rule) =>
        Object.freeze({
          id: rule.id,
          status: rule.status,
          filter: Object.freeze({ ...rule.filter }),
          expiration:
            rule.expiration === undefined
              ? undefined
              : Object.freeze({ ...rule.expiration }),
          abortIncompleteMultipartUpload:
            rule.abortIncompleteMultipartUpload === undefined
              ? undefined
              : Object.freeze({ ...rule.abortIncompleteMultipartUpload }),
        }),
      ),
    ),
  });
}
