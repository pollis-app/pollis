/**
 * The manifest: what an App asks for. Written in code, inside `app({ manifest })`,
 * and emitted as `pollis-app.json` by `pollis app build`, so it cannot drift from
 * what the code expects. An admin can narrow it when approving the App, never
 * widen it, and the runtime enforces it.
 *
 * The rules here mirror the runtime's (`pollis-app-manifest`) so a mistake is
 * reported at build time; the build then checks the emitted file with the
 * runtime's own parser too.
 */

/** What can wake an App. Each maps to one handler. */
export type Trigger = "messages" | "mentions" | "commands" | "schedule" | "requests" | "webhook";

/** What an App may do in the group. There are no moderation actions. */
export type Action = "post" | "reply" | "react" | "edit_own" | "delete_own" | "transfer_history";

/** Message event kinds an App may see. */
export type EventKind = "created" | "edited";

/** A slash command members can pick in the composer. */
export interface CommandSpec {
  /** Typed after the slash: lowercase letters, digits, '-' and '_', starting with a letter, up to 32 characters. */
  name: string;
  /** One line (up to 100 characters) shown beside the name. */
  description: string;
  /** What to type after the name, shown greyed (`"<title>"`). Leave it out for a command without arguments. */
  arg_hint?: string;
  /** Deliver privately to the bot (`onRequest`, trigger `requests`) instead of posting in the channel (`onCommand`). */
  request?: boolean;
}

/** When `onSchedule` runs: at every Unix time `k * every + offset` (UTC seconds). */
export interface ScheduleSpec {
  /** Lowercase letters, digits and '-', up to 32 characters; passed to `onSchedule`. */
  name: string;
  /** Period in seconds, 60 (a minute) to 604800 (a week). */
  every: number;
  /** Seconds after each period boundary, less than `every`. Default 0. */
  offset?: number;
}

export interface Manifest {
  /** Lowercase letters, digits and '-'. Default: the package name, without its scope. */
  name?: string;
  /** Default: the package version. */
  version?: string;
  /** Default: the package description. */
  description?: string;
  /** Default: one trigger per handler you define. If given, it must match your handlers. */
  triggers?: Trigger[];
  read?: {
    /** `"#name"` for one channel, or `"*"` for whichever channels the admin grants. */
    channels?: string[];
    /** Days of history `ctx.messages()` may reach back. Default 0: none. */
    history?: number;
    /** Default `["created"]`. Add `"edited"` to also see edits. */
    events?: EventKind[];
  };
  actions?: Action[];
  state?: {
    /** Bytes of keys plus values. Default 0: no state. */
    quota?: number;
  };
  /** Exact host names `ctx.fetch` may reach. Empty: the App cannot send data outside the group. */
  egress?: string[];
  /** Slash commands (up to 25). Needed for `onCommand`; a `request` command needs `onRequest`. */
  commands?: CommandSpec[];
  /** When `onSchedule` runs. Needed for `onSchedule`. */
  schedules?: ScheduleSpec[];
}

/** The manifest exactly as `pollis-app.json` holds it. */
export interface ResolvedManifest {
  name: string;
  version: string;
  description: string;
  triggers: Trigger[];
  read: { channels: string[]; history: number; events: EventKind[] };
  actions: Action[];
  state: { quota: number };
  egress: string[];
  /** Present only when not empty. */
  commands?: CommandSpec[];
  /** Present only when not empty. */
  schedules?: ScheduleSpec[];
}

export const TRIGGERS: readonly Trigger[] = ["messages", "mentions", "commands", "schedule", "requests", "webhook"];
export const ACTIONS: readonly Action[] = ["post", "reply", "react", "edit_own", "delete_own", "transfer_history"];
export const EVENTS: readonly EventKind[] = ["created", "edited"];

/** Which handler each trigger calls. */
export const HANDLER_FOR: Readonly<Record<Trigger, string>> = {
  messages: "onMessage",
  mentions: "onMention",
  commands: "onCommand",
  schedule: "onSchedule",
  requests: "onRequest",
  webhook: "onWebhook",
};

/** A mistake in the manifest, with the field it is about. */
export class ManifestError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ManifestError";
  }
}

const NAME = /^[a-z0-9][a-z0-9-]{0,63}$/;
const COMMAND_NAME = /^[a-z][a-z0-9_-]{0,31}$/;
const SCHEDULE_NAME = /^[a-z0-9-]{1,32}$/;
export const MAX_COMMANDS = 25;
export const MIN_SCHEDULE_SECS = 60;
export const MAX_SCHEDULE_SECS = 7 * 24 * 3600;
const CHANNEL = /^(\*|#?[^\s*#][^\s*]*)$/;
const HOST = /^(?=.{1,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]([a-z0-9-]{0,61}[a-z0-9])?$/;

/** The parts of package.json the manifest defaults come from. */
export interface PackageInfo {
  name?: string;
  version?: string;
  description?: string;
}

/** A package name as an App name: `@acme/Linear_Bot` becomes `linear-bot`. */
export function nameFromPackage(pkg: string | undefined): string | undefined {
  if (!pkg) {
    return undefined;
  }
  const bare = pkg.replace(/^@[^/]+\//, "").toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+/, "");
  return bare.slice(0, 64) || undefined;
}

function list<T extends string>(field: string, value: unknown, allowed?: readonly T[]): T[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value) || value.some((v) => typeof v !== "string")) {
    throw new ManifestError(`\`${field}\` must be a list of strings`);
  }
  const out: T[] = [];
  for (const v of value as string[]) {
    if (allowed && !allowed.includes(v as T)) {
      throw new ManifestError(`\`${field}\` has ${JSON.stringify(v)}; use one of ${allowed.map((a) => JSON.stringify(a)).join(", ")}`);
    }
    // The runtime reads these lists as sets: a repeat is harmless.
    if (!out.includes(v as T)) {
      out.push(v as T);
    }
  }
  return out;
}

function count(field: string, value: unknown): number {
  if (value === undefined) {
    return 0;
  }
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new ManifestError(`\`${field}\` must be a whole number, 0 or more`);
  }
  return value;
}

function only(field: string, value: object, keys: string[]): void {
  for (const key of Object.keys(value)) {
    if (!keys.includes(key)) {
      throw new ManifestError(`\`${field ? field + "." : ""}${key}\` is not a manifest field; the fields are ${keys.map((k) => `\`${k}\``).join(", ")}`);
    }
  }
}

function oneLine(text: unknown, max: number): boolean {
  if (typeof text !== "string") {
    return false;
  }
  const t = text.trim();
  return t.length > 0 && [...t].length <= max && !t.includes("\n");
}

function hintOk(text: string, max: number): boolean {
  return text.trim().length > 0 && [...text].length <= max && !text.includes("\n");
}

function checkCommands(triggers: Trigger[], commands: CommandSpec[]): void {
  if (triggers.includes("commands") && !commands.some((c) => !c.request)) {
    throw new ManifestError("`commands`: the `commands` trigger needs at least one command that is not a `request`");
  }
  if (commands.length > MAX_COMMANDS) {
    throw new ManifestError(`\`commands\`: at most ${MAX_COMMANDS} commands`);
  }
  const seen = new Set<string>();
  for (const c of commands) {
    if (typeof c.name !== "string" || !COMMAND_NAME.test(c.name)) {
      throw new ManifestError(
        `\`commands\`: name ${JSON.stringify(c.name)} is not valid: lowercase letters, digits, '-' and '_', 1 to 32 characters, starting with a letter`,
      );
    }
    if (seen.has(c.name)) {
      throw new ManifestError(`\`commands\`: /${c.name} is declared twice`);
    }
    seen.add(c.name);
    if (!oneLine(c.description, 100)) {
      throw new ManifestError(`\`commands\`: /${c.name}: \`description\` is one line of 1 to 100 characters`);
    }
    if (c.arg_hint !== undefined && (typeof c.arg_hint !== "string" || !hintOk(c.arg_hint, 60))) {
      throw new ManifestError(
        `\`commands\`: /${c.name}: \`arg_hint\` is one line of 1 to 60 characters (leave it out for no arguments)`,
      );
    }
    const needs: Trigger = c.request ? "requests" : "commands";
    if (!triggers.includes(needs)) {
      throw new ManifestError(
        `\`commands\`: /${c.name} needs \`${needs}\` in \`triggers\`${c.request ? " (it is a `request` command)" : ""}`,
      );
    }
  }
}

function checkSchedules(triggers: Trigger[], schedules: ScheduleSpec[]): void {
  const bad = (why: string) => new ManifestError(`\`schedules\` is not valid: ${why}`);
  const declared = triggers.includes("schedule");
  if (declared && schedules.length === 0) {
    throw bad("the `schedule` trigger needs at least one entry in `schedules`");
  }
  if (!declared && schedules.length > 0) {
    throw bad("`schedules` needs the `schedule` trigger in `triggers`");
  }
  const names = new Set<string>();
  for (const s of schedules) {
    if (typeof s.name !== "string" || !SCHEDULE_NAME.test(s.name)) {
      throw bad(`name ${JSON.stringify(s.name)}: lowercase letters, digits and '-', 1 to 32 characters`);
    }
    if (names.has(s.name)) {
      throw bad(`name ${JSON.stringify(s.name)} appears twice`);
    }
    names.add(s.name);
    if (!Number.isSafeInteger(s.every) || s.every < MIN_SCHEDULE_SECS || s.every > MAX_SCHEDULE_SECS) {
      throw bad(`${JSON.stringify(s.name)}: \`every\` is ${MIN_SCHEDULE_SECS} to ${MAX_SCHEDULE_SECS} seconds`);
    }
    if (!Number.isSafeInteger(s.offset ?? 0) || (s.offset ?? 0) < 0) {
      throw bad(`${JSON.stringify(s.name)}: \`offset\` must be a whole number of seconds`);
    }
    if ((s.offset ?? 0) >= s.every) {
      throw bad(`${JSON.stringify(s.name)}: \`offset\` must be less than \`every\``);
    }
  }
}

function commandList(value: unknown): CommandSpec[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new ManifestError("`commands` must be a list of { name, description, arg_hint?, request? }");
  }
  return value.map((c) => {
    if (c === null || typeof c !== "object") {
      throw new ManifestError("`commands` entries must be objects { name, description, arg_hint?, request? }");
    }
    only("commands[]", c, ["name", "description", "arg_hint", "request"]);
    if (c.request !== undefined && typeof c.request !== "boolean") {
      throw new ManifestError("`commands[].request` must be true or false");
    }
    const out: CommandSpec = { name: c.name, description: c.description };
    if (c.arg_hint !== undefined) {
      out.arg_hint = c.arg_hint;
    }
    if (c.request) {
      out.request = true;
    }
    return out;
  });
}

function scheduleList(value: unknown): ScheduleSpec[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new ManifestError("`schedules` must be a list of { name, every, offset? }");
  }
  return value.map((s) => {
    if (s === null || typeof s !== "object") {
      throw new ManifestError("`schedules` entries must be objects { name, every, offset? }");
    }
    only("schedules[]", s, ["name", "every", "offset"]);
    return { name: s.name, every: s.every, offset: s.offset ?? 0 };
  });
}

const FIELDS = ["name", "version", "description", "triggers", "read", "actions", "state", "egress", "commands", "schedules"];

/**
 * Check a manifest exactly as the runtime does (`Manifest::from_json` in
 * pollis-app-manifest), in the same order and with the same messages. The
 * input is the manifest as `pollis-app.json` holds it.
 */
export function validateManifest(json: unknown): ResolvedManifest {
  if (json === null || typeof json !== "object" || Array.isArray(json)) {
    throw new ManifestError("`pollis-app.json` must be an object");
  }
  const m = json as Record<string, unknown>;
  only("", m, ["$schema", ...FIELDS]);
  for (const required of ["name", "version"]) {
    if (typeof m[required] !== "string") {
      throw new ManifestError(`\`${required}\` is missing`);
    }
  }
  const read = (m.read ?? {}) as Record<string, unknown>;
  if (typeof read !== "object" || read === null) {
    throw new ManifestError("`read` must be an object");
  }
  only("read", read, ["channels", "history", "events"]);
  const state = (m.state ?? {}) as Record<string, unknown>;
  only("state", state, ["quota"]);
  if (m.description !== undefined && typeof m.description !== "string") {
    throw new ManifestError("`description` must be a string");
  }
  const triggers = list("triggers", m.triggers, TRIGGERS);
  const channels = list<string>("read.channels", read.channels);
  const events = read.events === undefined ? (["created"] as EventKind[]) : list("read.events", read.events, EVENTS);
  const actions = list("actions", m.actions, ACTIONS);
  const egress = list<string>("egress", m.egress);
  const history = count("read.history", read.history);
  if (history > 0xffffffff) {
    throw new ManifestError("`read.history` is too large");
  }
  const quota = count("state.quota", state.quota);
  const commands = commandList(m.commands);
  const schedules = scheduleList(m.schedules);

  const name = m.name as string;
  if (!NAME.test(name)) {
    throw new ManifestError(
      `\`name\` ${JSON.stringify(name)} is not valid: lowercase letters, digits and '-', 1 to 64 characters, starting with a letter or digit`,
    );
  }
  if ((m.version as string).trim() === "") {
    throw new ManifestError("`version` must not be empty");
  }
  for (const c of channels) {
    if (!CHANNEL.test(c)) {
      throw new ManifestError(`\`read.channels\` entry ${JSON.stringify(c)} is not valid: use "#name" or "*"`);
    }
  }
  for (const host of egress) {
    if (!HOST.test(host)) {
      throw new ManifestError(
        `\`egress\` entry ${JSON.stringify(host)} is not valid: an exact lowercase host name such as "api.example.com", with no scheme, port, path, wildcard or IP address`,
      );
    }
  }
  checkCommands(triggers, commands);
  checkSchedules(triggers, schedules);

  const out: ResolvedManifest = {
    name,
    version: m.version as string,
    description: (m.description as string | undefined) ?? "",
    triggers,
    read: { channels, history, events },
    actions,
    state: { quota },
    egress,
  };
  if (commands.length > 0) {
    out.commands = commands;
  }
  if (schedules.length > 0) {
    out.schedules = schedules;
  }
  return out;
}

/**
 * Fill in defaults from package.json and the handlers, check that triggers
 * and handlers agree, then check every rule the runtime applies
 * ({@link validateManifest}). Returns the manifest as it will be written.
 * `handlers` are the handler names the App defines.
 */
export function resolveManifest(manifest: Manifest, handlers: string[], pkg: PackageInfo = {}): ResolvedManifest {
  if (manifest === null || typeof manifest !== "object") {
    throw new ManifestError("`manifest` must be an object");
  }
  only("", manifest, FIELDS);

  const name = manifest.name ?? nameFromPackage(pkg.name);
  if (!name) {
    throw new ManifestError("`name` is missing: set it in the manifest or in package.json");
  }
  const version = manifest.version ?? pkg.version;
  if (version === undefined) {
    throw new ManifestError("`version` is missing: set it in the manifest or in package.json");
  }

  const defined = TRIGGERS.filter((t) => handlers.includes(HANDLER_FOR[t]));
  let triggers: Trigger[];
  if (manifest.triggers === undefined) {
    triggers = defined;
  } else {
    triggers = list("triggers", manifest.triggers, TRIGGERS);
    for (const t of triggers) {
      if (!handlers.includes(HANDLER_FOR[t])) {
        throw new ManifestError(`\`triggers\` has "${t}" but the App has no \`${HANDLER_FOR[t]}\` handler`);
      }
    }
    for (const t of defined) {
      if (!triggers.includes(t)) {
        throw new ManifestError(`\`${HANDLER_FOR[t]}\` would never run: add "${t}" to \`triggers\` (or leave \`triggers\` out to derive it from your handlers)`);
      }
    }
  }
  if (triggers.length === 0) {
    throw new ManifestError("the App has no handlers: define at least one of onMessage, onMention, onCommand, onSchedule, onRequest, onWebhook");
  }
  if (handlers.includes("onCommand") && !(manifest.commands ?? []).some((c) => !c?.request)) {
    throw new ManifestError(
      '`onCommand` needs its commands declared: add `commands: [{ name: "hello", description: "Say hello" }]` to the manifest',
    );
  }
  if (handlers.includes("onSchedule") && (manifest.schedules ?? []).length === 0) {
    throw new ManifestError('`onSchedule` needs `schedules`, e.g. `schedules: [{ name: "daily", every: 86400 }]`');
  }

  const full: Record<string, unknown> = {
    name,
    version,
    description: manifest.description ?? pkg.description ?? "",
    triggers,
    read: manifest.read ?? {},
    actions: manifest.actions,
    state: manifest.state ?? {},
    egress: manifest.egress,
    commands: manifest.commands,
    schedules: manifest.schedules,
  };
  return validateManifest(full);
}
