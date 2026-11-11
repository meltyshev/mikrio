import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { constants, createReadStream } from "node:fs";
import {
  chmod,
  copyFile,
  lstat,
  mkdir,
  readdir,
  realpath,
} from "node:fs/promises";
import { dirname, join, relative, resolve, sep } from "node:path";

interface Entry {
  kind: "directory" | "file";
  mode: number;
  size?: number;
  sha256?: string;
}

export type Inventory = Record<string, Entry>;

/** Inspect every entry, including empty directories and SQLite sidecars. */
export async function inspectInventory(root: string): Promise<Inventory> {
  const entries = Object.create(null) as Inventory;

  const visit = async (name: string): Promise<void> => {
    const path = join(root, name);
    const metadata = await lstat(path);
    const mode = metadata.mode & 0o777;

    if (metadata.isDirectory()) {
      entries[name] = { kind: "directory", mode };

      for (const child of (await readdir(path)).sort()) {
        await visit(join(name, child));
      }

      return;
    }

    assert.ok(
      metadata.isFile(),
      "backup input contains a link or special file",
    );

    const hash = createHash("sha256");

    for await (const chunk of createReadStream(path)) {
      assert.ok(Buffer.isBuffer(chunk));
      hash.update(chunk);
    }

    entries[name] = {
      kind: "file",
      mode,
      size: metadata.size,
      sha256: hash.digest("hex"),
    };
  };

  await visit(".");
  return entries;
}

/** The caller owns quiescence; matching manifests alone cannot prove it. */
export async function verifyCopy(
  source: string,
  destination: string,
  expected: Inventory,
): Promise<void> {
  assert.deepEqual(
    await inspectInventory(source),
    expected,
    "frozen source changed",
  );

  assert.deepEqual(
    await inspectInventory(destination),
    expected,
    "copy differs",
  );

  for (const [name, entry] of Object.entries(expected)) {
    if (entry.kind !== "file") {
      continue;
    }

    const original = await lstat(join(source, name));
    const copied = await lstat(join(destination, name));

    assert.ok(
      original.dev !== copied.dev || original.ino !== copied.ino,
      "copy shares a mutable file identity",
    );
  }
}

/** Copy only a closed, caller-owned dataset into a new independent directory. */
export async function copyOffline(
  source: string,
  destination: string,
): Promise<Inventory> {
  const original = await realpath(source);
  const parent = await realpath(dirname(destination));
  const target = resolve(parent, relative(dirname(destination), destination));
  const inside = relative(original, target);

  assert.ok(
    inside === ".." || inside.startsWith(".." + sep),
    "copy destination must be outside the source",
  );

  const expected = await inspectInventory(source);
  assert.equal(expected["."]?.kind, "directory");
  await mkdir(target, { mode: 0o700 });

  for (const [name, entry] of Object.entries(expected)) {
    if (name === ".") {
      continue;
    }

    const path = join(target, name);

    if (entry.kind === "directory") {
      await mkdir(path, { mode: 0o700 });
    } else {
      await copyFile(join(source, name), path, constants.COPYFILE_EXCL);
    }
  }

  // Apply directory modes last so restrictive parents cannot hide children.
  for (const [name, entry] of Object.entries(expected).reverse()) {
    await chmod(join(target, name), entry.mode);
  }

  await verifyCopy(source, target, expected);
  return expected;
}
