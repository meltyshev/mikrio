import { record, string } from "./inputs.cjs";
import type { CorsRule, GetBucketCorsOutput } from "./values.cjs";

function strings(value: unknown): string[] {
  if (!Array.isArray(value)) {
    throw new TypeError("CORS fields must be arrays");
  }

  return value.map((item: unknown) => string(item));
}

/** Own the complete caller input before registering asynchronous work. */
export function corsRules(value: unknown): CorsRule[] {
  if (!Array.isArray(value)) {
    throw new TypeError("corsRules must be an array");
  }

  return value.map((rule: unknown) => {
    const fields = record(
      rule,
      [
        "id",
        "allowedOrigins",
        "allowedMethods",
        "allowedHeaders",
        "exposeHeaders",
        "maxAgeSeconds",
      ],
      "CorsRule",
    );

    const seconds = fields.maxAgeSeconds;

    const hasInvalidMaxAge =
      seconds !== undefined &&
      (typeof seconds !== "number" || !Number.isSafeInteger(seconds));

    if (hasInvalidMaxAge) {
      throw new TypeError("maxAgeSeconds must be an integer");
    }

    return {
      id: fields.id === undefined ? undefined : string(fields.id),
      allowedOrigins: strings(fields.allowedOrigins),
      allowedMethods: strings(fields.allowedMethods),
      allowedHeaders:
        fields.allowedHeaders === undefined
          ? undefined
          : strings(fields.allowedHeaders),
      exposeHeaders:
        fields.exposeHeaders === undefined
          ? undefined
          : strings(fields.exposeHeaders),
      maxAgeSeconds: seconds,
    };
  });
}

/** Detach and freeze every nested array in a read result. */
export function corsOutput(value: GetBucketCorsOutput): GetBucketCorsOutput {
  return Object.freeze({
    corsRules: Object.freeze(
      value.corsRules.map((rule) =>
        Object.freeze({
          id: rule.id,
          allowedOrigins: Object.freeze([...rule.allowedOrigins]),
          allowedMethods: Object.freeze([...rule.allowedMethods]),
          allowedHeaders:
            rule.allowedHeaders === undefined
              ? undefined
              : Object.freeze([...rule.allowedHeaders]),
          exposeHeaders:
            rule.exposeHeaders === undefined
              ? undefined
              : Object.freeze([...rule.exposeHeaders]),
          maxAgeSeconds: rule.maxAgeSeconds,
        }),
      ),
    ),
  });
}
