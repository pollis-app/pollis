/**
 * `@pollis/app`: write a Pollis App in TypeScript.
 *
 * An App is a default export of {@link app}: a manifest saying what it needs,
 * and handlers the bot runtime calls when something happens in the group.
 * `pollis app build` compiles it into a sandboxed WebAssembly component and
 * writes the manifest beside it as `pollis-app.json`.
 *
 * ```ts
 * import { app } from "@pollis/app";
 *
 * export default app({
 *   manifest: { read: { channels: ["#support"] }, actions: ["reply"] },
 *   async onCommand(cmd, ctx) {
 *     await ctx.reply(cmd.message, `Hello, ${cmd.message.author.displayName}!`);
 *   },
 * });
 * ```
 *
 * Everything here is types plus the `app()` marker, so it imports cleanly in
 * Node (the build reads the manifest that way). The code that talks to the
 * runtime is `@pollis/app/runtime`, which only the built component loads.
 */

export type {
  Action,
  EventKind,
  Manifest,
  ResolvedManifest,
  Trigger,
} from "./manifest.js";
import type { Manifest } from "./manifest.js";

/** Who sent a message. */
export interface Author {
  /** Opaque user id. */
  id: string;
  displayName: string;
  /** True for bots, including this one. */
  isBot: boolean;
}

/** Attachment metadata. Apps never receive attachment bytes. */
export interface Attachment {
  name: string;
  mimeType: string;
  /** Bytes. */
  size: number;
}

/** A message in a channel the App was granted. */
export interface Message {
  /** Opaque message id. */
  id: string;
  /** Opaque channel id. Pass it to `ctx.post` to answer in the same channel. */
  channel: string;
  author: Author;
  text: string;
  attachments: Attachment[];
  /** The message this one replies to, if any. */
  replyTo?: string;
  /** The thread root this message belongs to, if any. */
  thread?: string;
  /** Unix milliseconds. */
  sentAt: number;
  /** Unix milliseconds of the last edit, if edited. */
  editedAt?: number;
}

/** A slash command addressed to this App: `/name args`. */
export interface Command {
  /** The command name, without the slash. */
  name: string;
  /** Everything after the name, unparsed. */
  args: string;
  /** The message that carried the command. Reply to it to answer. */
  message: Message;
}

/** A schedule firing. */
export interface Schedule {
  name: string;
  /** The slot this firing is for, Unix milliseconds. */
  slot: number;
}

/** A request a member sent to this App. */
export interface Request {
  id: string;
  requester: Author;
  body: string;
}

/** An inbound webhook delivery. */
export interface Webhook {
  deliveryId: string;
  headers: [string, string][];
  body: Uint8Array;
  /** The body as UTF-8 text. */
  text(): string;
  /** The body parsed as JSON. */
  json<T = unknown>(): T;
}

/** What `ctx.messages()` returns. All fields optional. */
export interface MessageQuery {
  /** One channel id; all granted channels when absent. */
  channel?: string;
  /** Inclusive. Clamped to the history window the App was granted. */
  since?: Date | number;
  /** Exclusive. */
  until?: Date | number;
  /** At most this many (default 50; the runtime may return fewer). */
  limit?: number;
}

/** The request half of `ctx.fetch`, shaped like the web's `RequestInit`. */
export interface FetchInit {
  method?: string;
  headers?: Record<string, string> | [string, string][] | Iterable<[string, string]>;
  body?: string | Uint8Array | ArrayBuffer;
}

/** Response headers, shaped like the web's `Headers` (read-only). */
export interface FetchHeaders extends Iterable<[string, string]> {
  get(name: string): string | null;
  has(name: string): boolean;
  forEach(fn: (value: string, name: string) => void): void;
  entries(): IterableIterator<[string, string]>;
}

/** The response half of `ctx.fetch`, shaped like the web's `Response`. */
export interface FetchResponse {
  readonly status: number;
  /** `status` is 200 to 299. */
  readonly ok: boolean;
  readonly headers: FetchHeaders;
  text(): Promise<string>;
  json<T = unknown>(): Promise<T>;
  bytes(): Promise<Uint8Array>;
  arrayBuffer(): Promise<ArrayBuffer>;
}

/**
 * The installation's key-value state: sealed, shared by every host that runs
 * the bot, limited to the manifest's `state.quota` bytes. Writes are kept only
 * if the handler finishes without throwing.
 */
export interface State {
  get(key: string): Promise<Uint8Array | undefined>;
  getText(key: string): Promise<string | undefined>;
  getJSON<T = unknown>(key: string): Promise<T | undefined>;
  set(key: string, value: string | Uint8Array): Promise<void>;
  setJSON(key: string, value: unknown): Promise<void>;
  delete(key: string): Promise<void>;
}

/** The runtime's log (`pollis-bot` output). Never shown in the group. */
export interface Logger {
  trace(text: string): void;
  debug(text: string): void;
  info(text: string): void;
  warn(text: string): void;
  error(text: string): void;
}

/**
 * What a handler can do. Every call is checked against what the admin
 * granted; a refused call rejects with a {@link RefusedError} whose message
 * names the manifest field, e.g. "`http-fetch` to `api.x.com` refused: not
 * in `egress`".
 *
 * Messages you post, reply, react, edit or delete are queued, and sent only
 * after the handler finishes without throwing. If it throws, nothing is sent
 * and no state is written.
 */
export interface Context {
  /** Post a new message in a channel (by id, e.g. `message.channel`). Needs `post`. */
  post(channel: string, text: string): Promise<void>;
  /** Reply to a message. Needs `reply`. */
  reply(to: Message | string, text: string): Promise<void>;
  /** React to a message with an emoji. Needs `react`. */
  react(to: Message | string, emoji: string): Promise<void>;
  /** Edit a message this bot sent. Needs `edit_own`. */
  editOwn(message: Message | string, text: string): Promise<void>;
  /** Delete a message this bot sent. Needs `delete_own`. */
  deleteOwn(message: Message | string): Promise<void>;
  /** Messages from granted channels within the granted history window (`read.history`). */
  messages(query?: MessageQuery): Promise<Message[]>;
  /** The installation's key-value state. Needs `state.quota` above 0. */
  state: State;
  /**
   * One HTTPS request to a host listed in `egress`, shaped like the web's
   * `fetch`. Redirects are followed only within the same host.
   */
  fetch(url: string | URL, init?: FetchInit): Promise<FetchResponse>;
  log: Logger;
  /** Unix milliseconds, from the runtime's clock. */
  now(): number;
}

/** The handlers an App may define. Each needs its trigger in the manifest. */
export interface Handlers {
  /** Every new message in a granted channel. Trigger: `messages`. */
  onMessage?(message: Message, ctx: Context): void | Promise<void>;
  /** A message that mentions the bot. Trigger: `mentions`. */
  onMention?(message: Message, ctx: Context): void | Promise<void>;
  /** A slash command addressed to the App. Trigger: `commands`. */
  onCommand?(command: Command, ctx: Context): void | Promise<void>;
  /** A schedule firing. Trigger: `schedule`. */
  onSchedule?(schedule: Schedule, ctx: Context): void | Promise<void>;
  /** A member's request. Trigger: `requests`. */
  onRequest?(request: Request, ctx: Context): void | Promise<void>;
  /** An inbound webhook. Trigger: `webhook`. */
  onWebhook?(webhook: Webhook, ctx: Context): void | Promise<void>;
}

/** An App: its manifest and its handlers. */
export interface AppDefinition extends Handlers {
  manifest: Manifest;
}

const APP_MARK = Symbol.for("pollis.app");

/** A host function refused a call. The message says which manifest field refused it. */
export class RefusedError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "RefusedError";
  }
}

/**
 * Define an App. Use it as the module's default export:
 * `export default app({ manifest: {...}, onCommand(cmd, ctx) {...} })`.
 */
export function app<T extends AppDefinition>(definition: T): T {
  if (definition === null || typeof definition !== "object") {
    throw new TypeError("app() takes an object: { manifest, onMessage, onCommand, ... }");
  }
  if (definition.manifest === null || typeof definition.manifest !== "object") {
    throw new TypeError("app() needs a `manifest` object");
  }
  Object.defineProperty(definition, APP_MARK, { value: true });
  return definition;
}

/** Whether a value came from {@link app}. */
export function isApp(value: unknown): value is AppDefinition {
  return typeof value === "object" && value !== null && (value as Record<symbol, unknown>)[APP_MARK] === true;
}
