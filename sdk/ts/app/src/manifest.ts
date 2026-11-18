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
    if (out.includes(v as T)) {
      throw new ManifestError(`\`${field}\` lists ${JSON.stringify(v)} twice`);
    }
    out.push(v as T);
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

/**
 * Fill in defaults, check every rule, and return the manifest as it will be
 * written. `handlers` are the handler names the App defines.
 */
export function resolveManifest(manifest: Manifest, handlers: string[], pkg: PackageInfo = {}): ResolvedManifest {
  if (manifest === null || typeof manifest !== "object") {
    throw new ManifestError("`manifest` must be an object");
  }
  only("", manifest, ["name", "version", "description", "triggers", "read", "actions", "state", "egress"]);
  const read = manifest.read ?? {};
  only("read", read, ["channels", "history", "events"]);
  const state = manifest.state ?? {};
  only("state", state, ["quota"]);

  const name = manifest.name ?? nameFromPackage(pkg.name);
  if (!name || !NAME.test(name)) {
    throw new ManifestError(
      `\`name\` ${JSON.stringify(name ?? "")} is not valid: lowercase letters, digits and '-', 1 to 64 characters, starting with a letter or digit`,
    );
  }
  const version = manifest.version ?? pkg.version ?? "";
  if (typeof version !== "string" || version.trim() === "") {
    throw new ManifestError("`version` is missing: set it in the manifest or in package.json");
  }
  const description = manifest.description ?? pkg.description ?? "";

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

  const channels = list<string>("read.channels", read.channels);
  for (const c of channels) {
    if (!CHANNEL.test(c)) {
      throw new ManifestError(`\`read.channels\` entry ${JSON.stringify(c)} is not valid: use "#name" or "*"`);
    }
  }
  const events = read.events === undefined ? (["created"] as EventKind[]) : list("read.events", read.events, EVENTS);
  const egress = list<string>("egress", manifest.egress);
  for (const host of egress) {
    if (!HOST.test(host)) {
      throw new ManifestError(
        `\`egress\` entry ${JSON.stringify(host)} is not valid: an exact lowercase host name such as "api.example.com", with no scheme, port, path, wildcard or IP address`,
      );
    }
  }

  return {
    name,
    version,
    description,
    triggers,
    read: { channels, history: count("read.history", read.history), events },
    actions: list("actions", manifest.actions, ACTIONS),
    state: { quota: count("state.quota", state.quota) },
    egress,
  };
}
