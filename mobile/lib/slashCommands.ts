/*
 * PORT of frontend/src/utils/slashCommands.ts (#2018); pollis-app-manifest/src/commands.rs is the
 * source of truth for both. frontend/tests/slash-commands.test.ts fails if the two drift.
 */

/** One command the group's Apps offer: `list_group_commands`, camelCase like every bot command. */
export interface GroupCommand {
  installationId: string;
  botUserId: string;
  /** Inserted as `@botUsername` after the command when `needsTarget`. */
  botUsername: string;
  /** The App's manifest name, shown beside a command two Apps declare. */
  appName: string;
  /** Without the slash. */
  command: string;
  description: string;
  argHint?: string | null;
  /** Delivered privately to the bot; never posted in the channel. */
  request: boolean;
  /** Channel ids the App is granted; the command is offered only there. */
  channels: string[];
  /** Another App declares the same name. */
  needsTarget: boolean;
}

const NAME_RE = /^[a-z][a-z0-9_-]{0,31}$/;

/** Max suggestions shown at once, like the mention list. */
const COMMAND_SUGGESTION_LIMIT = 8;

/** A message's text read as a command, or null. */
interface ParsedCommand {
  /** Lowercased. */
  name: string;
  /** The `@handle` right after the name, without '@'. */
  target: string | null;
  args: string;
}

/** Read `text` as a command (leading whitespace ignored); null for anything else. */
function parseCommand(text: string): ParsedCommand | null {
  const trimmed = text.replace(/^\s+/, "");
  if (!trimmed.startsWith("/")) {
    return null;
  }
  const rest = trimmed.slice(1);
  const end = rest.search(/\s/);
  const name = (end < 0 ? rest : rest.slice(0, end)).toLowerCase();
  if (!NAME_RE.test(name)) {
    return null;
  }
  const after = (end < 0 ? "" : rest.slice(end)).replace(/^\s+/, "");
  const tokenEnd = after.search(/\s/);
  const token = tokenEnd < 0 ? after : after.slice(0, tokenEnd);
  if (token.startsWith("@") && token.length > 1) {
    return { name, target: token.slice(1), args: (tokenEnd < 0 ? "" : after.slice(tokenEnd)).trim() };
  }
  return { name, target: null, args: after.trim() };
}

/** The `/…` the caret is typing: only as the message's first word. */
export interface CommandQuery {
  /** Lowercased, without the slash. */
  query: string;
  /** Index one past the typed name. */
  end: number;
}

export function commandQueryAt(text: string, caret: number): CommandQuery | null {
  if (!text.startsWith("/") || caret < 1) {
    return null;
  }
  const typed = text.slice(1, caret);
  if (/\s/.test(typed) || typed.includes("/")) {
    return null;
  }
  const rest = text.slice(caret);
  const tail = rest.search(/\s/);
  const end = caret + (tail < 0 ? rest.length : tail);
  return { query: typed.toLowerCase(), end };
}

/** The commands offered in `channelId`. */
export function commandsForChannel(commands: GroupCommand[], channelId: string | null): GroupCommand[] {
  if (!channelId) {
    return [];
  }
  return commands.filter((c) => c.channels.includes(channelId));
}

/** Commands matching `query`: name prefix first, then name or description containing it. */
export function rankCommands(
  commands: GroupCommand[],
  query: string,
  limit: number = COMMAND_SUGGESTION_LIMIT,
): GroupCommand[] {
  const q = query.toLowerCase();
  const scored: Array<{ c: GroupCommand; rank: number }> = [];
  for (const c of commands) {
    if (c.command.startsWith(q)) {
      scored.push({ c, rank: 0 });
    } else if (q && (c.command.includes(q) || c.description.toLowerCase().includes(q))) {
      scored.push({ c, rank: 1 });
    }
  }
  scored.sort((a, b) =>
    a.rank !== b.rank
      ? a.rank - b.rank
      : a.c.command.localeCompare(b.c.command) || a.c.appName.localeCompare(b.c.appName),
  );
  return scored.slice(0, limit).map((s) => s.c);
}

/** A stable key for one App's command (two Apps may share a name). */
export function commandKey(c: GroupCommand): string {
  return `${c.command}:${c.installationId}`;
}

/** Replace the typed `/name` with the picked command (and its bot when ambiguous), keeping what follows. */
export function applyCommand(text: string, caret: number, picked: GroupCommand): { text: string; caret: number } {
  const q = commandQueryAt(text, caret);
  if (!q) {
    return { text, caret };
  }
  const head = picked.needsTarget ? `/${picked.command} @${picked.botUsername} ` : `/${picked.command} `;
  const rest = text.slice(q.end).replace(/^\s+/, "");
  return { text: head + rest, caret: head.length };
}

/** What the composer does with a message on send. */
export type CommandSend =
  | { kind: "post" }
  | { kind: "request"; command: GroupCommand; args: string }
  | { kind: "ambiguous"; name: string };

/**
 * Decide how `text` is sent. A `request` command is never posted (checked against every
 * command of the group, not only this channel's); a name two Apps here declare needs the
 * bot's `@handle`; anything else is an ordinary message, including unknown commands.
 */
export function classifyCommandSend(
  text: string,
  commands: GroupCommand[],
  channelId: string | null,
): CommandSend {
  const parsed = parseCommand(text);
  if (!parsed) {
    return { kind: "post" };
  }
  const target = parsed.target?.toLowerCase() ?? null;
  const matches = commands.filter(
    (c) => c.command === parsed.name && (target === null || c.botUsername.toLowerCase() === target),
  );
  if (matches.length === 0) {
    return { kind: "post" };
  }
  const here = channelId ? matches.filter((c) => c.channels.includes(channelId)) : [];
  const pool = here.length > 0 ? here : matches;
  if (pool.length > 1) {
    return { kind: "ambiguous", name: parsed.name };
  }
  const chosen = pool[0];
  if (chosen.request) {
    return { kind: "request", command: chosen, args: parsed.args };
  }
  return { kind: "post" };
}

/** The request command `text` would send, if it is one: the composer says it stays private. */
export function pendingRequestCommand(
  text: string,
  commands: GroupCommand[],
  channelId: string | null,
): GroupCommand | null {
  const send = classifyCommandSend(text, commands, channelId);
  return send.kind === "request" ? send.command : null;
}

/** The core's refusal while the request transport (#2008) is not available. */
export const BOT_REQUESTS_UNAVAILABLE = "bot_requests_unavailable";
