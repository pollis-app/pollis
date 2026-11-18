/**
 * `@pollis/app/testing`: run your built App offline, in your test files.
 *
 * Each call runs one handler in the same sandbox the bot runtime uses, with
 * the same permission checks and the same error messages, so a call your
 * manifest does not allow fails in a test exactly as it would in a group.
 * Nothing touches the network: `ctx.fetch` answers from the `http` fixtures
 * you give, and state starts from the `state` you give.
 *
 * ```ts
 * import { test } from "node:test";
 * import assert from "node:assert/strict";
 * import { testApp } from "@pollis/app/testing";
 *
 * const app = await testApp();
 *
 * test("/hello replies", async () => {
 *   const result = await app.command("hello", "world");
 *   assert.deepEqual(result.replies, ["Hello, world!"]);
 * });
 * ```
 *
 * `pollis app test` builds the App and runs your `*.test.ts` files with this
 * wired up. It needs the `pollis-app-dev` program (see the developer guide).
 */

import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createInterface } from "node:readline";

import type { Author, Message } from "./index.js";
import type { ResolvedManifest } from "./manifest.js";

/** A canned answer for `ctx.fetch`, matched by exact URL (and method, if given). */
export interface HttpFixture {
  url: string;
  method?: string;
  /** Default 200. */
  status?: number;
  headers?: Record<string, string> | [string, string][];
  /** A string, bytes, or any other value (sent as JSON). */
  body?: string | Uint8Array | unknown;
}

/** What the admin granted, if you want to test a narrower installation than the manifest asks for. */
export interface Grants {
  /** Channel name to channel id, e.g. `{ support: "support" }`. */
  channels?: Record<string, string>;
  history?: number;
  events?: ("created" | "edited")[];
  actions?: string[];
  egress?: string[];
}

export interface JobOptions {
  /** State before the job, by key. */
  state?: Record<string, string | Uint8Array>;
  /** Answers for `ctx.fetch`. A request with no fixture fails, as an unreachable host would. */
  http?: HttpFixture[];
  /** History `ctx.messages()` can return (still limited to what is granted). */
  messages?: MessageInput[];
  /** Default: everything the manifest asks for. */
  grants?: Grants;
  /** The runtime's clock, Unix milliseconds or a Date. Default 2027-01-15T08:00:00Z. */
  now?: number | Date;
}

export interface TestOptions extends JobOptions {
  /** Default: `$POLLIS_APP_COMPONENT`, else `dist/app.wasm`. */
  component?: string;
  /** Default: `$POLLIS_APP_MANIFEST`, else `dist/pollis-app.json`. */
  manifest?: string;
  /** The `pollis-app-dev` program. Default: `$POLLIS_APP_DEV_BIN`, else `pollis-app-dev` on PATH. */
  bin?: string;
}

/** A message as tests describe it: everything optional. */
export interface MessageInput {
  id?: string;
  /** Channel id. In tests a channel's id is its name: `"support"` for `#support`. */
  channel?: string;
  author?: Partial<Author>;
  text?: string;
  replyTo?: string;
  thread?: string;
  sentAt?: number | Date;
  editedAt?: number | Date;
}

export type Action =
  | { type: "post"; channel: string; text: string }
  | { type: "reply"; channel: string; message: string; text: string }
  | { type: "react"; channel: string; message: string; emoji: string }
  | { type: "editOwn"; channel: string; message: string; text: string }
  | { type: "deleteOwn"; channel: string; message: string };

/** What one handler run produced. */
export interface JobResult {
  /** The handler finished without throwing, and nothing was refused that made it throw. */
  ok: boolean;
  /** Why the job failed: `kind` is e.g. `handler_failed`, `not_delivered`, `timeout`, `trap`. */
  error?: { kind: string; message: string };
  /** Everything the App queued, in order. Empty when the job failed: nothing would be sent. */
  actions: Action[];
  /** Texts of `reply` actions. */
  replies: string[];
  /** `post` actions. */
  posts: { channel: string; text: string }[];
  /** Emoji of `react` actions. */
  reactions: string[];
  /** State written by the job, as text (`null`: deleted). */
  state: Record<string, string | null>;
  /** Every `ctx.log` line and console output. */
  logs: { level: string; text: string }[];
  /** Every `ctx.fetch` that passed the egress check. */
  fetches: { method: string; url: string; headers: [string, string][]; body?: string }[];
}

/** A built App, ready to run in tests. */
export interface TestApp {
  readonly manifest: ResolvedManifest;
  message(message: string | MessageInput, options?: JobOptions): Promise<JobResult>;
  mention(message: string | MessageInput, options?: JobOptions): Promise<JobResult>;
  command(name: string, args?: string, options?: JobOptions & { message?: MessageInput }): Promise<JobResult>;
  schedule(name: string, options?: JobOptions & { slot?: number | Date }): Promise<JobResult>;
  request(body: string, options?: JobOptions & { id?: string; requester?: Partial<Author> }): Promise<JobResult>;
  webhook(body: string | Uint8Array | unknown, options?: JobOptions & { headers?: Record<string, string>; deliveryId?: string }): Promise<JobResult>;
  /** Stop the runner. Optional: it also stops when the test process exits. */
  close(): void;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();
/** 2027-01-15T08:00:00Z, the default clock. */
export const DEFAULT_NOW = 1_800_000_000_000;

function bytes(value: string | Uint8Array | unknown): number[] {
  if (value instanceof Uint8Array) {
    return Array.from(value);
  }
  if (typeof value === "string") {
    return Array.from(encoder.encode(value));
  }
  return Array.from(encoder.encode(JSON.stringify(value)));
}

function msOf(value: number | Date | undefined, fallback: number): number {
  if (value === undefined) {
    return fallback;
  }
  return value instanceof Date ? value.getTime() : value;
}

/** Start the runner for a built App. */
export async function testApp(options: TestOptions = {}): Promise<TestApp> {
  const component = resolve(options.component ?? process.env.POLLIS_APP_COMPONENT ?? "dist/app.wasm");
  const manifestPath = resolve(options.manifest ?? process.env.POLLIS_APP_MANIFEST ?? "dist/pollis-app.json");
  let manifest: ResolvedManifest;
  try {
    manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as ResolvedManifest;
  } catch (e) {
    throw new Error(`no built App at ${manifestPath}: run \`pollis app build\` first, or run your tests with \`pollis app test\` (${(e as Error).message})`);
  }
  const bin = options.bin ?? process.env.POLLIS_APP_DEV_BIN ?? "pollis-app-dev";
  const defaults: JobOptions = options;
  const defaultChannel =
    manifest.read.channels.find((c) => c !== "*")?.replace(/^#/, "") ?? "general";

  let child: ChildProcessWithoutNullStreams | undefined;
  let failure: Error | undefined;
  const waiting: { resolve: (line: string) => void; reject: (e: Error) => void }[] = [];
  let ids = 0;

  type Handle = { ref?: () => void; unref?: () => void };
  /** While a job is in flight the runner keeps the test process alive; idle, it does not. */
  function hold(proc: ChildProcessWithoutNullStreams, held: boolean): void {
    for (const h of [proc, proc.stdin, proc.stdout, proc.stderr] as Handle[]) {
      if (held) {
        h.ref?.();
      } else {
        h.unref?.();
      }
    }
  }

  function idle(): void {
    if (child && waiting.length === 0) {
      hold(child, false);
    }
  }

  function start(): ChildProcessWithoutNullStreams {
    if (child) {
      return child;
    }
    const proc = spawn(bin, ["serve"], { stdio: ["pipe", "pipe", "pipe"] });
    let stderr = "";
    proc.stderr.on("data", (d) => {
      stderr += String(d);
    });
    proc.on("error", (e) => {
      const err =
        (e as NodeJS.ErrnoException).code === "ENOENT"
          ? new Error(`\`${bin}\` not found: install it or set POLLIS_APP_DEV_BIN (the developer guide's "Testing" section says how)`)
          : e;
      failure = err;
      for (const w of waiting.splice(0)) {
        w.reject(err);
      }
    });
    proc.on("exit", (code) => {
      const err = new Error(`\`${bin} serve\` exited (${code}): ${stderr.trim()}`);
      failure = err;
      for (const w of waiting.splice(0)) {
        w.reject(err);
      }
    });
    createInterface({ input: proc.stdout }).on("line", (line) => {
      const w = waiting.shift();
      w?.resolve(line);
      idle();
    });
    process.once("exit", () => proc.kill());
    child = proc;
    return proc;
  }

  function message(input: string | MessageInput | undefined, now: number): Record<string, unknown> {
    const m: MessageInput = typeof input === "string" ? { text: input } : (input ?? {});
    ids += 1;
    return {
      id: m.id ?? `m${ids}`,
      channel: m.channel ?? defaultChannel,
      author: {
        id: m.author?.id ?? "u-alice",
        displayName: m.author?.displayName ?? "Alice",
        isBot: m.author?.isBot ?? false,
      },
      text: m.text ?? "",
      replyTo: m.replyTo,
      thread: m.thread,
      sentAt: msOf(m.sentAt, now),
      editedAt: m.editedAt === undefined ? undefined : msOf(m.editedAt, now),
    };
  }

  function grants(g: Grants | undefined): Record<string, unknown> | undefined {
    if (!g) {
      return undefined;
    }
    return {
      channels: Object.entries(g.channels ?? {}).map(([name, id]) => ({ id, name: name.replace(/^#/, "") })),
      history: g.history ?? 0,
      events: g.events ?? ["created"],
      actions: g.actions ?? [],
      egress: g.egress ?? [],
    };
  }

  async function job(trigger: Record<string, unknown>, opts: JobOptions | undefined): Promise<JobResult> {
    const o: JobOptions = { ...defaults, ...opts };
    const now = msOf(o.now, DEFAULT_NOW);
    const request = {
      component,
      manifest: manifestPath,
      trigger,
      grants: grants(o.grants),
      messages: (o.messages ?? []).map((m) => message(m, now)),
      state: Object.fromEntries(Object.entries(o.state ?? {}).map(([k, v]) => [k, bytes(v)])),
      http: (o.http ?? []).map((f) => ({
        url: f.url,
        method: f.method,
        status: f.status ?? 200,
        headers: Array.isArray(f.headers) ? f.headers : Object.entries(f.headers ?? {}),
        body: f.body === undefined ? [] : bytes(f.body),
      })),
      now,
    };
    if (failure) {
      throw failure;
    }
    const proc = start();
    hold(proc, true);
    const line = await new Promise<string>((resolveLine, reject) => {
      waiting.push({ resolve: resolveLine, reject });
      proc.stdin.write(JSON.stringify(request) + "\n");
    });
    return toResult(JSON.parse(line));
  }

  const now = () => msOf(defaults.now, DEFAULT_NOW);
  return {
    manifest,
    message: (m, o) => job({ message: message(m, msOf(o?.now, now())) }, o),
    mention: (m, o) => job({ mention: message(m, msOf(o?.now, now())) }, o),
    command: (name, args = "", o) =>
      job(
        {
          command: {
            name,
            args,
            message: message({ text: `/${name}${args ? " " + args : ""}`, ...o?.message }, msOf(o?.now, now())),
          },
        },
        o,
      ),
    schedule: (name, o) => job({ schedule: { name, slot: msOf(o?.slot, msOf(o?.now, now())) } }, o),
    request: (body, o) =>
      job(
        {
          request: {
            id: o?.id ?? `r${++ids}`,
            requester: {
              id: o?.requester?.id ?? "u-alice",
              displayName: o?.requester?.displayName ?? "Alice",
              isBot: o?.requester?.isBot ?? false,
            },
            body,
          },
        },
        o,
      ),
    webhook: (body, o) =>
      job(
        {
          webhook: {
            deliveryId: o?.deliveryId ?? `d${++ids}`,
            headers: Object.entries(o?.headers ?? {}),
            body: bytes(body),
          },
        },
        o,
      ),
    close: () => {
      child?.stdin.end();
      child?.kill();
      child = undefined;
    },
  };
}

interface WireResult {
  ok: boolean;
  error?: { kind: string; message: string };
  actions: Action[];
  stateWrites: { key: string; value: number[] | null }[];
  logs: { level: string; text: string }[];
  fetches: { method: string; url: string; headers: [string, string][]; body: number[] | null }[];
}

function toResult(wire: WireResult): JobResult {
  const actions = wire.actions;
  return {
    ok: wire.ok,
    error: wire.error,
    actions,
    replies: actions.flatMap((a) => (a.type === "reply" ? [a.text] : [])),
    posts: actions.flatMap((a) => (a.type === "post" ? [{ channel: a.channel, text: a.text }] : [])),
    reactions: actions.flatMap((a) => (a.type === "react" ? [a.emoji] : [])),
    state: Object.fromEntries(
      wire.stateWrites.map((w) => [w.key, w.value === null ? null : decoder.decode(Uint8Array.from(w.value))]),
    ),
    logs: wire.logs,
    fetches: wire.fetches.map((f) => ({
      method: f.method,
      url: f.url,
      headers: f.headers,
      body: f.body === null ? undefined : decoder.decode(Uint8Array.from(f.body)),
    })),
  };
}

export type { Message };
