import {
  createHash,
  randomBytes,
  randomUUID,
  scrypt,
  timingSafeEqual,
} from "node:crypto";
import { AppError, parseText, type Account } from "./model.mjs";
import type { State } from "./state.mjs";

/** Local accounts are application identities, unrelated to S3 principals. */
export class Auth {
  #activeLogins = 0;
  #loginTokens = 20;
  #refilledAtMs = performance.now();

  constructor(readonly state: State) {}

  async addAccount(username: unknown, password: unknown): Promise<void> {
    const name = parseUsername(username);
    const secret = parsePassword(password);
    const salt = randomBytes(16).toString("hex");
    const passwordHash = await hashPassword(secret, salt);

    await this.state.call("addAccount", {
      accountId: randomUUID(),
      username: name,
      salt,
      passwordHash: passwordHash.toString("hex"),
    });
  }

  async login(username: unknown, password: unknown): Promise<string> {
    const name = parseUsername(username);
    const secret = parsePassword(password);
    const now = performance.now();

    this.#loginTokens = Math.min(
      20,
      this.#loginTokens + (now - this.#refilledAtMs) / 3000,
    );

    this.#refilledAtMs = now;

    if (this.#activeLogins >= 2 || this.#loginTokens < 1) {
      throw new AppError("unavailable");
    }

    this.#loginTokens -= 1;
    this.#activeLogins += 1;

    try {
      const account = await this.state.call("account", { username: name });

      const actual = await hashPassword(
        secret,
        account?.salt ?? "00".repeat(16),
      );

      const expected = Buffer.from(
        account?.passwordHash ?? "00".repeat(32),
        "hex",
      );

      if (!timingSafeEqual(actual, expected) || !account) {
        throw new AppError("unauthorized");
      }

      const token = randomBytes(32).toString("hex");

      await this.state.call("addSession", {
        accountId: account.accountId,
        tokenHash: hashToken(token),
        now: Date.now(),
      });

      return token;
    } finally {
      this.#activeLogins -= 1;
    }
  }

  async requireSession(
    token: string | undefined,
  ): Promise<Pick<Account, "accountId" | "username">> {
    if (!token || !/^[0-9a-f]{64}$/u.test(token)) {
      throw new AppError("unauthorized");
    }

    const session = await this.state.call("session", {
      tokenHash: hashToken(token),
      now: Date.now(),
    });

    if (!session) {
      throw new AppError("unauthorized");
    }

    return session;
  }

  async logout(token: string): Promise<void> {
    await this.state.call("logout", { tokenHash: hashToken(token) });
  }
}

function parseUsername(value: unknown): string {
  const name = parseText(value, 64);

  if (!/^[a-z0-9][a-z0-9._-]{2,63}$/u.test(name)) {
    throw new AppError("invalid");
  }

  return name;
}

function parsePassword(value: unknown): string {
  const password = parseText(value, 1024);

  if (password.length < 12) {
    throw new AppError("invalid");
  }

  return password;
}

function hashPassword(password: string, salt: string): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    scrypt(
      password,
      Buffer.from(salt, "hex"),
      32,
      { N: 131072, r: 8, p: 1, maxmem: 192 * 1024 * 1024 },
      (error, key) => {
        if (error) {
          reject(new AppError("unavailable"));
          return;
        }

        resolve(key);
      },
    );
  });
}

function hashToken(token: string): string {
  return createHash("sha256").update(token).digest("hex");
}
