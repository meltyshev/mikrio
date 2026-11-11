import assert from "node:assert/strict";
import { execFile, fork } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { copyOffline, inspectInventory, verifyCopy } from "./offline-copy.mjs";

await test("unclean offline app and engine copies retain both WALs and recover attachments and multipart state in another process", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "mikrio-unclean-restore-"));

  t.after(async () => {
    if (t.passed) {
      await rm(root, { recursive: true });
    } else {
      t.diagnostic(`retained unclean restore: ${root}`);
    }
  });

  const source = join(root, "application");
  const expected = join(root, "expected.json");

  const script = fileURLToPath(
    new URL("./unclean-recovery-child.mjs", import.meta.url),
  );

  const child = fork(script, ["write", source, expected], {
    execArgv: ["--no-global-search-paths"],
    silent: true,
  });

  const stopped = new Promise<NodeJS.Signals | null>((resolve) => {
    child.once("close", (_code, signal) => resolve(signal));
  });

  const deadline = AbortSignal.timeout(30_000);
  let output = "";

  for (const stream of [child.stdout, child.stderr]) {
    assert.ok(stream);

    stream.setEncoding("utf8").on("data", (chunk: string) => {
      if (output.length > 64 * 1024) {
        child.kill("SIGKILL");
        return;
      }

      output += chunk;
    });
  }

  try {
    const messages: readonly unknown[] = await once(child, "message", {
      signal: deadline,
    });

    assert.equal(messages[0], "ready");
  } finally {
    // A copy is permitted only after the OS reports termination and closes all
    // inherited output handles; a requested signal alone is not quiescence.
    child.kill("SIGKILL");
    assert.equal(await stopped, "SIGKILL");
  }

  assert.equal(output, "");
  const backup = join(root, "backup");
  const manifest = await copyOffline(source, backup);

  for (const name of ["store/metadata.sqlite3-wal", "app.sqlite3-wal"]) {
    assert.ok(
      (manifest[name]?.size ?? 0) > 32,
      "backup must retain real WAL frames",
    );

    const incomplete = join(
      root,
      name.startsWith("store/") ? "missing-engine-wal" : "missing-app-wal",
    );

    await copyOffline(backup, incomplete);
    await rm(join(incomplete, name));

    await assert.rejects(
      verifyCopy(source, incomplete, manifest),
      /copy differs/u,
    );
  }

  const working = join(root, "working");
  await copyOffline(backup, working);

  const restored = await promisify(execFile)(
    process.execPath,
    ["--no-global-search-paths", script, "verify", working, expected],
    { timeout: 30_000, killSignal: "SIGKILL", maxBuffer: 64 * 1024 },
  );

  assert.equal(restored.stdout, "verified\n");
  assert.equal(restored.stderr, "");
  assert.deepEqual(await inspectInventory(source), manifest);
  assert.deepEqual(await inspectInventory(backup), manifest);
});
