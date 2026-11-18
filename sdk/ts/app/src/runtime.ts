/// <reference path="./generated/app.d.ts" />
/**
 * The bridge between an App's handlers and the bot runtime's host functions
 * (`pollis:app/host@0.1.0`). Only the built component loads this module:
 * `pollis app build` generates an entry that calls {@link exportsFor} on the
 * App's default export and exports the six WIT handlers.
 *
 * Host calls are synchronous; the context presents them as promises so App
 * code reads like ordinary async TypeScript. A host refusal becomes a
 * rejected promise ({@link RefusedError}); a handler that throws or rejects
 * makes the job fail, and the runtime then applies none of its queued actions
 * or state writes.
 */

import * as host from "pollis:app/host@0.1.0";
import type * as wit from "pollis:app/types@0.1.0";

import {
  RefusedError,
  type AppDefinition,
  type Command,
  type Context,
  type FetchHeaders,
  type FetchInit,
  type FetchResponse,
  type Message,
  type MessageQuery,
  type Request,
  type Schedule,
  type Webhook,
} from "./index.js";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** The text of an error thrown by a host import or by App code. */
function errorText(e: unknown): string {
  if (typeof e === "string") {
    return e;
  }
  if (e && typeof e === "object") {
    const payload = (e as { payload?: unknown }).payload;
    if (typeof payload === "string") {
      return payload;
    }
    const message = (e as { message?: unknown }).message;
    if (typeof message === "string" && message) {
      return message;
    }
  }
  return String(e);
}

/** Call a host import that returns `result<T, string>`. */
function call<T>(fn: () => T): Promise<T> {
  try {
    return Promise.resolve(fn());
  } catch (e) {
    return Promise.reject(new RefusedError(errorText(e)));
  }
}

function ms(value: bigint | undefined): number | undefined {
  return value === undefined ? undefined : Number(value);
}

function toMessage(m: wit.Message): Message {
  return {
    id: m.id,
    channel: m.channel,
    author: { id: m.author.id, displayName: m.author.displayName, isBot: m.author.isBot },
    text: m.text,
    attachments: m.attachments.map((a) => ({ name: a.name, mimeType: a.mimeType, size: Number(a.size) })),
    replyTo: m.replyTo,
    thread: m.thread,
    sentAt: Number(m.sentAt),
    editedAt: ms(m.editedAt),
  };
}

function idOf(target: Message | string): string {
  return typeof target === "string" ? target : target.id;
}

function time(value: Date | number | undefined): bigint | undefined {
  if (value === undefined) {
    return undefined;
  }
  return BigInt(Math.floor(value instanceof Date ? value.getTime() : value));
}

function bodyBytes(body: FetchInit["body"]): Uint8Array | undefined {
  if (body === undefined) {
    return undefined;
  }
  if (typeof body === "string") {
    return encoder.encode(body);
  }
  if (body instanceof Uint8Array) {
    return body;
  }
  if (body instanceof ArrayBuffer) {
    return new Uint8Array(body);
  }
  throw new TypeError("ctx.fetch body must be a string, Uint8Array or ArrayBuffer");
}

function headerList(headers: FetchInit["headers"]): [string, string][] {
  if (!headers) {
    return [];
  }
  if (Symbol.iterator in Object(headers)) {
    return Array.from(headers as Iterable<[string, string]>, ([k, v]) => [String(k), String(v)]);
  }
  return Object.entries(headers as Record<string, string>).map(([k, v]) => [k, String(v)]);
}

class Headers implements FetchHeaders {
  readonly #list: [string, string][];
  constructor(list: [string, string][]) {
    this.#list = list.map(([k, v]) => [k.toLowerCase(), v]);
  }
  get(name: string): string | null {
    const n = name.toLowerCase();
    const values = this.#list.filter(([k]) => k === n).map(([, v]) => v);
    return values.length ? values.join(", ") : null;
  }
  has(name: string): boolean {
    return this.get(name) !== null;
  }
  forEach(fn: (value: string, name: string) => void): void {
    for (const [k, v] of this.#list) {
      fn(v, k);
    }
  }
  entries(): IterableIterator<[string, string]> {
    return this.#list[Symbol.iterator]();
  }
  [Symbol.iterator](): IterableIterator<[string, string]> {
    return this.entries();
  }
}

class Response implements FetchResponse {
  readonly status: number;
  readonly ok: boolean;
  readonly headers: FetchHeaders;
  readonly #body: Uint8Array;
  constructor(r: wit.HttpResponse) {
    this.status = r.status;
    this.ok = r.status >= 200 && r.status < 300;
    this.headers = new Headers(r.headers);
    this.#body = r.body;
  }
  async text(): Promise<string> {
    return decoder.decode(this.#body);
  }
  async json<T>(): Promise<T> {
    return JSON.parse(decoder.decode(this.#body)) as T;
  }
  async bytes(): Promise<Uint8Array> {
    return this.#body;
  }
  async arrayBuffer(): Promise<ArrayBuffer> {
    return this.#body.slice().buffer as ArrayBuffer;
  }
}

/** The context handed to every handler. */
export function createContext(): Context {
  return {
    post: (channel, text) => call(() => host.post(channel, text)),
    reply: (to, text) => call(() => host.reply(idOf(to), text)),
    react: (to, emoji) => call(() => host.react(idOf(to), emoji)),
    editOwn: (message, text) => call(() => host.editOwn(idOf(message), text)),
    deleteOwn: (message) => call(() => host.deleteOwn(idOf(message))),
    messages: (query: MessageQuery = {}) =>
      call(() =>
        host
          .queryMessages({
            channel: query.channel,
            since: time(query.since),
            until: time(query.until),
            limit: query.limit ?? 50,
          })
          .map(toMessage),
      ),
    state: {
      get: (key) => call(() => host.kvGet(key)),
      getText: (key) => call(() => host.kvGet(key)).then((v) => (v === undefined ? undefined : decoder.decode(v))),
      getJSON: <T>(key: string) =>
        call(() => host.kvGet(key)).then((v) => (v === undefined ? undefined : (JSON.parse(decoder.decode(v)) as T))),
      set: (key, value) => call(() => host.kvSet(key, typeof value === "string" ? encoder.encode(value) : value)),
      setJSON: (key, value) => call(() => host.kvSet(key, encoder.encode(JSON.stringify(value)))),
      delete: (key) => call(() => host.kvDelete(key)),
    },
    fetch: (url, init = {}) => {
      let request: wit.HttpRequest;
      try {
        request = {
          method: (init.method ?? "GET").toUpperCase(),
          url: String(url),
          headers: headerList(init.headers),
          body: bodyBytes(init.body),
        };
      } catch (e) {
        return Promise.reject(e);
      }
      return call(() => new Response(host.httpFetch(request)));
    },
    log: {
      trace: (text) => host.log("trace", String(text)),
      debug: (text) => host.log("debug", String(text)),
      info: (text) => host.log("info", String(text)),
      warn: (text) => host.log("warn", String(text)),
      error: (text) => host.log("error", String(text)),
    },
    now: () => Number(host.now()),
  };
}

/** The six WIT exports, each running the App's handler (if any) with a fresh context. */
export interface Exports {
  onMessage(message: wit.Message): Promise<void>;
  onMention(message: wit.Message): Promise<void>;
  onCommand(command: wit.Command): Promise<void>;
  onSchedule(schedule: wit.Schedule): Promise<void>;
  onRequest(request: wit.Request): Promise<void>;
  onWebhook(webhook: wit.Webhook): Promise<void>;
}

/**
 * Run a handler and settle as the WIT export must: resolve for ok, reject
 * with a string for err (the string becomes the job's error).
 */
async function run(name: string, handler: ((arg: never, ctx: Context) => unknown) | undefined, arg: unknown): Promise<void> {
  if (typeof handler !== "function") {
    throw `the App has no ${name} handler`;
  }
  try {
    await handler(arg as never, createContext());
  } catch (e) {
    throw errorText(e);
  }
}

export function exportsFor(definition: AppDefinition): Exports {
  const d = definition;
  return {
    onMessage: (m) => run("onMessage", d.onMessage, toMessage(m)),
    onMention: (m) => run("onMention", d.onMention, toMessage(m)),
    onCommand: (c) => {
      const command: Command = { name: c.name, args: c.args, message: toMessage(c.message) };
      return run("onCommand", d.onCommand, command);
    },
    onSchedule: (s) => {
      const schedule: Schedule = { name: s.name, slot: Number(s.slot) };
      return run("onSchedule", d.onSchedule, schedule);
    },
    onRequest: (r) => {
      const request: Request = {
        id: r.id,
        requester: { id: r.requester.id, displayName: r.requester.displayName, isBot: r.requester.isBot },
        body: r.body,
      };
      return run("onRequest", d.onRequest, request);
    },
    onWebhook: (w) => {
      const webhook: Webhook = {
        deliveryId: w.deliveryId,
        headers: w.headers,
        body: w.body,
        text: () => decoder.decode(w.body),
        json: <T>() => JSON.parse(decoder.decode(w.body)) as T,
      };
      return run("onWebhook", d.onWebhook, webhook);
    },
  };
}
