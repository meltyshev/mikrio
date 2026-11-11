import { Worker } from "node:worker_threads";
import {
  AppError,
  type ErrorCode,
  type StateCommands,
  type StateOptions,
} from "./model.mjs";

interface Pending {
  resolve: (value: unknown) => void;
  reject: (error: AppError) => void;
}

/** One bounded queue and one worker own the app database until explicit close. */
export class State {
  readonly #worker: Worker;
  readonly #pending = new Map<number, Pending>();
  readonly #exited: Promise<void>;
  readonly #ready: Promise<void>;
  #sequence = 0;
  #closing: Promise<void> | undefined;
  #failed = false;

  private constructor(options: StateOptions) {
    this.#worker = new Worker(new URL("./state-worker.mjs", import.meta.url), {
      workerData: options,
    });

    const ready = Promise.withResolvers<void>();
    this.#ready = ready.promise;

    this.#exited = new Promise((resolve) => {
      this.#worker.once("exit", () => {
        this.#fail();
        ready.reject(new AppError("unavailable"));
        resolve();
      });
    });

    this.#worker.on("error", () => {
      this.#fail();
      ready.reject(new AppError("unavailable"));
    });

    this.#worker.on(
      "message",
      (message: {
        ready?: boolean;
        id: number;
        result?: unknown;
        code?: ErrorCode;
      }) => {
        if (message.ready) {
          ready.resolve();
          return;
        }

        const pending = this.#pending.get(message.id);
        this.#pending.delete(message.id);

        if (message.code) {
          pending?.reject(new AppError(message.code));
        } else {
          pending?.resolve(message.result);
        }
      },
    );
  }

  static async open(options: StateOptions): Promise<State> {
    const state = new State(options);

    try {
      await state.#ready;
      return state;
    } catch (error) {
      await state.#exited;
      throw error;
    }
  }

  call<K extends keyof StateCommands>(
    command: K,
    input: StateCommands[K]["input"],
  ): Promise<StateCommands[K]["output"]> {
    if (this.#failed || this.#closing || this.#pending.size >= 64) {
      return Promise.reject(new AppError("unavailable"));
    }

    return this.#send(command, input);
  }

  close(): Promise<void> {
    this.#closing ??= this.#close();
    return this.#closing;
  }

  [Symbol.asyncDispose](): Promise<void> {
    return this.close();
  }

  async #close(): Promise<void> {
    try {
      if (this.#failed) {
        throw new AppError("unavailable");
      }

      await this.#send("close", undefined);
    } finally {
      await this.#exited;
    }
  }

  #send<K extends keyof StateCommands>(
    command: K,
    input: StateCommands[K]["input"],
  ): Promise<StateCommands[K]["output"]> {
    const id = ++this.#sequence;

    const reply = new Promise<unknown>((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });

      try {
        this.#worker.postMessage({ id, command, input });
      } catch {
        this.#pending.delete(id);
        reject(new AppError("unavailable"));
      }
    });

    // Both ends import the same private protocol. No HTTP value selects a command.
    return reply as Promise<StateCommands[K]["output"]>;
  }

  #fail(): void {
    this.#failed = true;

    for (const pending of this.#pending.values()) {
      pending.reject(new AppError("unavailable"));
    }

    this.#pending.clear();
  }
}
