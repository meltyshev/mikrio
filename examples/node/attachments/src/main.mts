import { once } from "node:events";
import { Auth } from "./auth.mjs";
import { Attachments } from "./attachments.mjs";
import { loadConfig, type Config } from "./config.mjs";
import { createApplicationServer } from "./http.mjs";
import { AppError } from "./model.mjs";
import { State } from "./state.mjs";
import { S3Storage } from "./storage.mjs";
import { Visibility } from "./visibility.mjs";
import { PostDemo } from "./post-demo.mjs";

async function main(): Promise<void> {
  const [command, path, argument] = process.argv.slice(2);

  if (!path || process.argv.length > 5) {
    throw new AppError("invalid");
  }

  if (command === "local-init" && argument === undefined) {
    const { initializeLocal } = await import("./local.mjs");
    const config = await initializeLocal(path);
    process.stdout.write(`Created private app configuration: ${config}\n`);
    return;
  }

  const config = await loadConfig(path);

  if (command === "local-reconcile-grant" && argument) {
    const { configureLocalReconciliation } = await import("./local.mjs");
    await configureLocalReconciliation(config, argument);
    process.stdout.write("Application backend prefix listing configured.\n");
    return;
  }

  if (command === "local-public-grant" && argument) {
    const { configureLocalPublication } = await import("./local.mjs");
    await configureLocalPublication(config, argument);

    process.stdout.write(
      "Public attachment policy configured. Enable publicAttachments in the private app configuration.\n",
    );

    return;
  }

  if (command === "init" && argument === undefined) {
    const state = await State.open({ ...config.state, initialize: true });
    await state.close();
    process.stdout.write("Application state initialized.\n");
    return;
  }

  const isServeCommand =
    command === "serve" &&
    (argument === undefined || argument === "--accept-authoritative-state");

  if (isServeCommand) {
    await serve(config, argument !== undefined);
    return;
  }

  if (command !== "account" || !argument) {
    throw new AppError("invalid");
  }

  await using state = await State.open(config.state);

  if (process.stdin.isTTY) {
    throw new AppError("invalid");
  }

  const chunks: Buffer[] = [];
  let receivedBytes = 0;

  for await (const chunk of process.stdin) {
    if (!Buffer.isBuffer(chunk)) {
      throw new AppError("invalid");
    }

    receivedBytes += chunk.length;

    if (receivedBytes > 1025) {
      throw new AppError("invalid");
    }

    chunks.push(chunk);
  }

  const password = new TextDecoder("utf-8", { fatal: true })
    .decode(Buffer.concat(chunks))
    .replace(/\r?\n$/u, "");

  await new Auth(state).addAccount(argument, password);
  process.stdout.write("Application account created.\n");
}

async function serve(config: Config, authoritative: boolean): Promise<void> {
  // Acquire in order and register cleanup immediately, including partial startup.
  await using resources = new AsyncDisposableStack();

  if (config.local) {
    const { Store } = await import("mikrio");
    const store = await Store.open({ directory: config.local.directory });
    resources.defer(() => store.close());

    const listener = await store.listen({
      host: "127.0.0.1",
      port: config.local.port,
      publicEndpoint: config.storage.endpoint,
    });

    resources.defer(() => listener.close());
  }

  const state = await State.open(config.state);
  resources.defer(() => state.close());
  const storage = new S3Storage(config.storage);
  resources.defer(() => storage.close());

  const attachments = new Attachments(
    state,
    storage,
    config.state.bucket,
    config.state.prefix,
    authoritative,
  );

  const visibility = new Visibility(
    state,
    storage,
    config.state.bucket,
    config.state.prefix,
    config.publicAttachments === true,
    authoritative,
  );

  let stopping = false;

  const postDemo = config.postDemo
    ? new PostDemo(
        storage,
        config.postDemo.bucket,
        config.postDemo.prefix,
        config.appOrigin,
        authoritative,
      )
    : undefined;

  let timer: ReturnType<typeof setTimeout> | undefined;
  let cleanup = Promise.resolve();

  const application = await createApplicationServer(
    new Auth(state),
    attachments,
    config.appOrigin,
    config.storage.endpoint,
    visibility,
    postDemo,
  );

  resources.defer(async () => {
    stopping = true;
    clearTimeout(timer);
    const results = await Promise.allSettled([application.close(), cleanup]);

    const failures = results
      .filter((result) => result.status === "rejected")
      .map((result) => result.reason as unknown);

    if (failures.length) {
      throw new AggregateError(failures, "application shutdown failed");
    }
  });

  const { server } = application;
  server.listen(config.appPort, "127.0.0.1");
  await once(server, "listening");

  const schedule = (): void => {
    timer = setTimeout(() => {
      cleanup = Promise.allSettled([
        attachments.cleanup(),
        visibility.enabled
          ? visibility.reconcile()
          : Promise.resolve({ failed: 0 }),
        postDemo ? postDemo.cleanup() : Promise.resolve({ failed: 0 }),
      ])
        .then(
          (results) => {
            const hasFailures = results.some(
              (result) =>
                result.status === "rejected" || result.value.failed > 0,
            );

            if (hasFailures) {
              process.stderr.write(
                "Attachment maintenance will retry failed storage requests.\n",
              );
            }
          },
          () => {
            process.stderr.write(
              "Attachment cleanup paused: authoritative state or storage traversal is unavailable.\n",
            );
          },
        )
        .finally(() => {
          if (!stopping) {
            schedule();
          }
        });
    }, 60_000);
  };

  if (authoritative) {
    schedule();
  }

  process.stdout.write(
    `Attachment app: ${config.appOrigin}\nCleanup ${authoritative ? "enabled for accepted authoritative state" : "paused"}.\n`,
  );

  await new Promise<void>((resolve) => {
    const stop = (): void => {
      process.off("SIGINT", stop);
      process.off("SIGTERM", stop);
      resolve();
    };

    process.once("SIGINT", stop);
    process.once("SIGTERM", stop);
  });
}

try {
  await main();
} catch {
  // Configuration/SDK errors may contain credentials. Report no exception dump.
  process.stderr.write(
    "Attachment app failed. Check the private configuration and state; existing data was retained.\n",
  );

  process.exitCode = 1;
}
