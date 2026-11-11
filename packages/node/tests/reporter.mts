import type { TestEvent } from "node:test/reporters";

const REPORTED_EVENTS = [
  "test:enqueue",
  "test:pass",
  "test:fail",
  "test:summary",
  "test:stderr",
  "test:diagnostic",
];

/** Preserve runner discovery and results, without inventing successful cells. */
export default async function* reporter(
  events: AsyncIterable<TestEvent>,
): AsyncGenerator<string> {
  for await (const event of events) {
    if (!REPORTED_EVENTS.includes(event.type)) {
      continue;
    }

    yield JSON.stringify(event, (_key: string, value: unknown): unknown => {
      if (value instanceof Error) {
        return {
          name: value.name,
          message: value.message,
          stack: value.stack,
          cause: value.cause,
          errors: "errors" in value ? value.errors : undefined,
          error: "error" in value ? value.error : undefined,
          suppressed: "suppressed" in value ? value.suppressed : undefined,
        };
      }

      return value;
    }) + "\n";
  }
}
