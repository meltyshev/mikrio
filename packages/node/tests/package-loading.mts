import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);
const publicPackage = dirname(dirname(require.resolve("@mikrio/node")));
const controls = mkdtempSync(join(process.cwd(), "package-loading-"));

// These are isolated copies of the installed wrapper. The sentinel is never an
// engine substitute: rejection must happen before its entry point executes.
for (const scenario of [
  "missing",
  "wrong-version",
  "wrong-name",
  "wrong-public-name",
] as const) {
  const consumer = join(controls, scenario);
  const publicCopy = join(consumer, "node_modules/@mikrio/node");
  cpSync(publicPackage, publicCopy, { recursive: true });

  const childRequire = createRequire(join(publicCopy, "package.json"));
  const target: unknown = childRequire("./dist/target.cjs");
  assert.ok(typeof target === "object" && target !== null);
  assert.ok("nativePackageName" in target);
  assert.equal(typeof target.nativePackageName, "function");
  assert.ok("usesGlibc" in target);
  assert.equal(typeof target.usesGlibc, "function");

  // Private compiled helpers are inspected only by this packaging control.
  const select = target.nativePackageName as (
    platform: string,
    architecture: string,
    glibc: boolean,
  ) => string;

  const glibc = target.usesGlibc as () => boolean;
  const name = select(process.platform, process.arch, glibc());

  for (const [platform, architecture, suffix] of [
    ["darwin", "arm64", "darwin-arm64"],
    ["darwin", "x64", "darwin-x64"],
    ["linux", "arm64", "linux-arm64-gnu"],
    ["linux", "x64", "linux-x64-gnu"],
    ["win32", "x64", "win32-x64-msvc"],
  ] as const) {
    assert.equal(
      select(platform, architecture, true),
      `@mikrio/node-${suffix}`,
    );
  }

  for (const [platform, architecture] of [
    ["linux", "x64"],
    ["linux", "arm64"],
    ["win32", "arm64"],
    ["freebsd", "x64"],
    ["darwin", "ia32"],
  ] as const) {
    assert.throws(
      () => select(platform, architecture, false),
      /Unsupported mikrio native target/,
    );
  }

  if (scenario !== "missing") {
    const manifest: unknown = childRequire("./package.json");
    assert.ok(typeof manifest === "object" && manifest !== null);
    assert.ok("version" in manifest && typeof manifest.version === "string");

    if (scenario === "wrong-public-name") {
      writeFileSync(
        join(publicCopy, "package.json"),
        JSON.stringify({ ...manifest, name: "@mikrio/wrong-package" }),
      );
    }

    const native = join(consumer, "node_modules", name);
    mkdirSync(native, { recursive: true });

    writeFileSync(
      join(native, "package.json"),
      JSON.stringify({
        name: scenario === "wrong-name" ? "@mikrio/wrong-package" : name,
        version:
          scenario === "wrong-version"
            ? manifest.version + "-wrong"
            : manifest.version,
        main: "./index.cjs",
      }),
    );

    writeFileSync(
      join(native, "index.cjs"),
      "throw new Error('native sentinel executed');\n",
    );
  }

  const child = spawnSync(
    process.execPath,
    [
      "--no-global-search-paths",
      "--input-type=module",
      "-e",
      `
        import assert from "node:assert/strict";
        import { existsSync } from "node:fs";
        import { createRequire } from "node:module";
        import { Store } from "@mikrio/node";

        const require = createRequire(import.meta.url);
        assert.equal(Store, require("@mikrio/node").Store);

        assert.equal(
          process.report
            .getReport()
            .sharedObjects.some((path) => path.endsWith("/mikrio.node")),
          false,
        );

        await assert.rejects(
          Store.open({ directory: "must-not-exist" }),
          /${scenario === "missing" ? "Missing" : "Mismatched"} mikrio native package/,
        );

        assert.equal(existsSync("must-not-exist"), false);
      `,
    ],
    { cwd: consumer, encoding: "utf8", timeout: 30_000 },
  );

  assert.ifError(child.error);
  assert.equal(child.status, 0, scenario + "\n" + child.stdout + child.stderr);
  process.stdout.write(`PASS ${scenario}\n`);
}
