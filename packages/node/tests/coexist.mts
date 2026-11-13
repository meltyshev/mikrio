import assert from "node:assert/strict";
import type { DatabaseSync } from "node:sqlite";

const [order, directory] = process.argv.slice(2);
assert.ok(directory);
assert.ok(order === "sqlite-first" || order === "engine-first");
let database: DatabaseSync | undefined;
const failures: unknown[] = [];

try {
  if (order === "sqlite-first") {
    const sqlite = await import("node:sqlite");
    database = new sqlite.DatabaseSync(":memory:");
  }

  const { Store } = await import("@mikrio/node");

  {
    await using store = await Store.open({ directory });

    if (order === "engine-first") {
      const sqlite = await import("node:sqlite");
      database = new sqlite.DatabaseSync(":memory:");
    }

    assert.ok(database);
    database.exec("CREATE TABLE separate_data(value BLOB)");

    database
      .prepare("INSERT INTO separate_data(value) VALUES (?)")
      .run(Buffer.from("independent"));

    await store.createBucket({ bucket: "coexist" });

    await store.putObject({
      bucket: "coexist",
      key: "object",
      body: Buffer.from("persistent"),
    });

    const row = database.prepare("SELECT value FROM separate_data").get();
    assert.ok(row?.value instanceof Uint8Array);
    assert.equal(Buffer.from(row.value).toString(), "independent");
  }

  await using reopened = await Store.open({ directory });
  const output = await reopened.getObject({ bucket: "coexist", key: "object" });
  await using body = output.body;

  assert.equal(
    Buffer.from(await body.readAll({ maxBytes: 10 })).toString(),
    "persistent",
  );
} catch (error) {
  failures.push(error);
}

try {
  database?.close();
} catch (error) {
  failures.push(error);
}

if (failures.length > 0) {
  throw new AggregateError(failures, "Native coexistence fixture failed");
}

process.stdout.write(`PASS ${order}\n`);
