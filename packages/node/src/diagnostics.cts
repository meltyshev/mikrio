import type { NativeDiagnostics } from "./native.cjs";

let receivers: NativeDiagnostics[] = [];
let timer: NodeJS.Timeout | undefined;
let output: ((message: string) => unknown) | undefined;
let busy = false;

/** One lazy, unreferenced timer per environment; no Store enters this registry. */
export function startDelivery(receiver: NativeDiagnostics): void {
  receivers = receivers.filter((current) => !current.isClosed());

  if (receivers.length === 0) {
    clearInterval(timer);
    timer = undefined;
  }

  if (!receivers.some((current) => current.sameQueue(receiver))) {
    receivers.push(receiver);
  }

  if (timer !== undefined) {
    return;
  }

  try {
    output = console.error.bind(console);
    timer = setInterval(poll, 250);
    timer.unref();
  } catch {
    receiver.observerFailed();
  }
}

function poll(): void {
  receivers = receivers.filter((receiver) => !receiver.isClosed());

  if (receivers.length === 0) {
    clearInterval(timer);
    timer = undefined;
    output = undefined;
    return;
  }

  if (busy || output === undefined) {
    return;
  }

  const receiver = receivers.shift();

  if (receiver === undefined) {
    return;
  }

  receivers.push(receiver);
  const message = receiver.take();

  if (message === null) {
    return;
  }

  busy = true;

  // The detached callback owns only its message and weak observation handle.
  // A host replacement may return a Promise; never accumulate callbacks behind it.
  void deliver(receiver, message, output);
}

async function deliver(
  receiver: NativeDiagnostics,
  message: string,
  write: (message: string) => unknown,
): Promise<void> {
  try {
    await write(message);
  } catch {
    receiver.observerFailed();
  } finally {
    busy = false;
  }
}
