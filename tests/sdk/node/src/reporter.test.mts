import assert from "node:assert/strict";
import { Readable } from "node:stream";
import { test } from "node:test";
import type { TestEvent } from "node:test/reporters";
import reporter from "./reporter.mjs";

await test("reporter retains discovery and nested primary and cleanup failures", async () => {
  const failure = new Error("owned primary failure");
  const cleanup = new Error("owned cleanup failure");
  const suppressed = new Error("owned suppressed failure");

  const aggregate = new AggregateError(
    [
      Object.assign(new Error("owned wrapper"), { error: failure, suppressed }),
      cleanup,
    ],
    "owned aggregate",
  );

  const identity = {
    name: "owned failing fixture",
    nesting: 0,
    parentId: undefined,
    tags: [],
    testId: 1,
  };

  const events: TestEvent[] = [
    { type: "test:enqueue", data: { ...identity, type: "test" } },
    {
      type: "test:fail",
      data: {
        ...identity,
        testNumber: 1,
        details: {
          duration_ms: 1,
          error: Object.assign(new Error("test failed"), { cause: aggregate }),
        },
      },
    },
  ];

  const lines: string[] = [];

  for await (const line of reporter(Readable.from(events))) {
    lines.push(line);
  }

  assert.equal(lines.length, 2);
  const output = lines.map((line): unknown => JSON.parse(line));

  // Stack frames and durations vary; discovery and nested error fields must survive.
  assert.partialDeepStrictEqual(output, [
    { type: "test:enqueue", data: { name: "owned failing fixture" } },
    {
      type: "test:fail",
      data: {
        name: "owned failing fixture",
        details: {
          error: {
            name: "Error",
            message: "test failed",
            cause: {
              name: "AggregateError",
              message: "owned aggregate",
              errors: [
                {
                  message: "owned wrapper",
                  error: { message: "owned primary failure" },
                  suppressed: { message: "owned suppressed failure" },
                },
                { message: "owned cleanup failure" },
              ],
            },
          },
        },
      },
    },
  ]);
});
