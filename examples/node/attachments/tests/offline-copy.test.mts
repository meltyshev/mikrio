import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { link, mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { copyOffline, inspectInventory, verifyCopy } from "./offline-copy.mjs";

await test("offline copy verifies hidden files and sidecars and rejects incomplete, changed, linked or overlapping copies", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "mikrio-copy-control-"));

  t.after(async () => {
    if (t.passed) {
      await rm(root, { recursive: true });
    } else {
      t.diagnostic(`retained copy control: ${root}`);
    }
  });

  const source = join(root, "source");
  await mkdir(source, { mode: 0o700 });
  await mkdir(join(source, "empty"), { mode: 0o700 });

  for (const name of [
    ".hidden",
    "__proto__",
    "constructor",
    "db",
    "db-wal",
    "db-shm",
    "guard",
  ]) {
    await writeFile(join(source, name), name, { mode: 0o600 });
  }

  const backup = join(root, "backup");
  const manifest = await copyOffline(source, backup);

  assert.deepEqual(Object.keys(manifest).sort(), [
    ".",
    ".hidden",
    "__proto__",
    "constructor",
    "db",
    "db-shm",
    "db-wal",
    "empty",
    "guard",
  ]);

  assert.equal(
    manifest["db-wal"]?.sha256,
    createHash("sha256").update("db-wal").digest("hex"),
  );

  await assert.rejects(copyOffline(source, source), /outside the source/u);

  await assert.rejects(
    copyOffline(source, join(source, "nested")),
    /outside the source/u,
  );

  await assert.rejects(copyOffline(source, backup), { code: "EEXIST" });
  assert.deepEqual(await inspectInventory(backup), manifest);

  const damaged = join(root, "damaged");
  await copyOffline(backup, damaged);
  await rm(join(damaged, "db-wal"));
  await assert.rejects(verifyCopy(backup, damaged, manifest), /copy differs/u);
  await writeFile(join(damaged, "db-wal"), "wrong!", { mode: 0o600 });
  await assert.rejects(verifyCopy(backup, damaged, manifest), /copy differs/u);
  await rm(join(damaged, "db-wal"));
  await link(join(backup, "db-wal"), join(damaged, "db-wal"));

  await assert.rejects(
    verifyCopy(backup, damaged, manifest),
    /mutable file identity/u,
  );

  await rm(join(damaged, "db-wal"));
  await symlink(join(backup, "db-wal"), join(damaged, "db-wal"));
  await assert.rejects(inspectInventory(damaged), /link or special file/u);
  assert.deepEqual(await inspectInventory(backup), manifest);

  await writeFile(join(source, "db"), "changed");

  await assert.rejects(
    verifyCopy(source, backup, manifest),
    /frozen source changed/u,
  );
});
