import targets from "./targets.json";

/** Select a packaged native target; unsupported systems have no fallback. */
export function nativePackageName(
  platform: string,
  architecture: string,
  glibc: boolean,
): string {
  const target = targets.find(
    (entry) =>
      entry.os === platform &&
      entry.cpu === architecture &&
      (entry.libc === undefined || glibc),
  );

  if (target === undefined) {
    const libc =
      platform === "linux" ? (glibc ? "/glibc" : "/unknown-libc") : "";

    throw new Error(
      `Unsupported mikrio native target: ${platform}/${architecture}${libc}`,
    );
  }

  return target.name;
}

/** Inspect libc only when opening a store on Linux, never during import. */
export function usesGlibc(): boolean {
  if (process.platform !== "linux") {
    return false;
  }

  const report: unknown = process.report.getReport();

  if (typeof report !== "object" || report === null || !("header" in report)) {
    return false;
  }

  const header: unknown = report.header;

  return (
    typeof header === "object" &&
    header !== null &&
    "glibcVersionRuntime" in header &&
    typeof header.glibcVersionRuntime === "string"
  );
}
