// Diagnostic delivery adapter for native probe tests.
import assert from "node:assert/strict";
import { createRequire } from "node:module";

export interface DiagnosticEvent {
  readonly storeId: string;
  readonly work: string;
  readonly failure?: string | undefined;
  readonly occurrences: bigint;
  readonly ageMs: bigint;
}

export interface DiagnosticProbe {
  emit(detail: string): void;
  take(): DiagnosticEvent | null;
  close(): void;
  observerFailed(): void;
  readonly stats: {
    queued: number;
    dropped: bigint;
    coalesced: bigint;
    observerFailures: bigint;
  };
}

export function loadProbe(path: string): DiagnosticProbe {
  const loaded: unknown = createRequire(import.meta.url)(path);
  assert.ok(typeof loaded === "object" && loaded !== null);

  assert.ok(
    "DiagnosticProbe" in loaded && typeof loaded.DiagnosticProbe === "function",
  );

  const native = loaded as { DiagnosticProbe: new () => DiagnosticProbe };
  return new native.DiagnosticProbe();
}

/** One fixed-rate host delivery loop; never awaited by native shutdown. */
export class ConsoleDelivery {
  private readonly timer: NodeJS.Timeout;
  private readonly output: (line: string) => unknown =
    console.error.bind(console);
  private closed = false;
  private busy = false;
  private polls = 0;

  constructor(private readonly probe: DiagnosticProbe) {
    this.timer = setInterval(() => {
      this.polls++;
      void this.drain();
    }, 250);

    this.timer.unref();
  }

  // Observation-only fixture handshake; not a shipping diagnostic API.
  get pollCount(): number {
    return this.polls;
  }

  close(): void {
    this.closed = true;
    clearInterval(this.timer);
    this.probe.close();
  }

  private async drain(): Promise<void> {
    if (this.closed || this.busy) {
      return;
    }

    this.busy = true;

    try {
      const event = this.probe.take();

      if (event === null) {
        return;
      }

      const stats = this.probe.stats;
      const line = `mikrio: storage work needs attention store_id=${event.storeId} work=${event.work} failure=${event.failure ?? "unknown"} occurrences=${event.occurrences} age_ms=${event.ageMs} dropped_observations=${stats.dropped} observer_failures=${stats.observerFailures}`;

      // Standard console.error returns void. Await also contains a recoverable
      // rejection from a host replacement, with at most one delivery in flight.
      await this.output(line);
    } catch {
      // Do not stringify or recursively report an arbitrary observer exception.
      this.probe.observerFailed();
    } finally {
      this.busy = false;
    }
  }
}
