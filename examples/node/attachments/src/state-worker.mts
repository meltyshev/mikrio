import { parentPort, workerData } from "node:worker_threads";
import { Database } from "./database.mjs";
import { AppError, type StateCommands, type StateOptions } from "./model.mjs";

type Request = {
  [K in keyof StateCommands]: {
    id: number;
    command: K;
    input: StateCommands[K]["input"];
  };
}[keyof StateCommands];

const port = parentPort;

if (!port) {
  throw new Error("database worker requires a parent");
}

const database = new Database(workerData as StateOptions);
port.postMessage({ ready: true });

port.on("message", (request: Request) => {
  try {
    port.postMessage({ id: request.id, result: dispatch(request) });
  } catch (error) {
    port.postMessage({
      id: request.id,
      code: error instanceof AppError ? error.code : "unavailable",
    });
  }

  if (request.command === "close") {
    port.close();
  }
});

function dispatch(request: Request): unknown {
  switch (request.command) {
    case "addAccount":
      return database.addAccount(request.input);
    case "account":
      return database.findAccount(request.input.username);
    case "addSession":
      return database.addSession(
        request.input.accountId,
        request.input.tokenHash,
        request.input.now,
      );
    case "session":
      return database.findSession(request.input.tokenHash, request.input.now);
    case "logout":
      return database.logout(request.input.tokenHash);
    case "createAttempt":
      return database.createAttempt(request.input);
    case "attempt":
      return database.getAttempt(
        request.input.attemptId,
        request.input.accountId,
      );
    case "attach":
      return database.attach(
        request.input.attemptId,
        request.input.accountId,
        request.input.now,
      );
    case "attachments":
      return database.listAttachments(request.input.accountId);
    case "attachment":
      return database.findAttachment(
        request.input.attemptId,
        request.input.accountId,
      );
    case "claimCleanup":
      return database.claimCleanup(request.input.now);
    case "setVisibility":
      return database.setVisibility(
        request.input.attemptId,
        request.input.accountId,
        request.input.visibility,
      );
    case "claimVisibility":
      return database.claimVisibility(request.input.now);
    case "visibilityWork":
      return database.getVisibilityWork(request.input.attemptId);
    case "finishVisibility":
      return database.finishVisibility(
        request.input.attemptId,
        request.input.revision,
        request.input.succeeded,
        request.input.now,
      );
    case "orphan":
      return database.isOrphan(request.input.key);
    case "compact":
      return database.compact();
    case "close":
      return database.close();
  }
}
