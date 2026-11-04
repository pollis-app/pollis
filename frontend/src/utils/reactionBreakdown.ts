// Who reacted with what (the reaction pill tooltip and the "View reactions" panel); pure.
// mobile/lib/reactionBreakdown.ts is a copy (mobile imports no frontend TS); tests/reaction-breakdown.test.ts diffs them.

export interface ReactionInput {
  emoji: string;
  // Oldest reaction first: the DS reads `message_reaction` ORDER BY created_at.
  user_ids: readonly string[];
}

/** What a roster or `get_user_profiles` knows about one user. */
export interface ReactorProfile {
  username?: string | null;
  preferred_name?: string | null;
  avatar_url?: string | null;
}

export interface Reactor {
  userId: string;
  isSelf: boolean;
  // Null: nobody could name them (left, deleted, never resolved) — the UI says "Unknown user".
  name: string | null;
  // The username under a display name that differs from it; null otherwise.
  handle: string | null;
  // Undefined: no source said; null: known to have no picture.
  avatarKey: string | null | undefined;
  // The emojis this person reacted with, in tab order (the All tab lists them).
  emojis: string[];
}

/** One tab: an emoji, or the All tab (`emoji: null`). */
export interface ReactionTab {
  key: string;
  emoji: string | null;
  count: number;
  people: Reactor[];
}

export const ALL_TAB_KEY = "all";

/** Every reactor once, in tab order then reaction order. */
export function reactorIds(reactions: readonly ReactionInput[]): string[] {
  const seen = new Set<string>();
  for (const reaction of reactions) {
    for (const id of reaction.user_ids) {
      seen.add(id);
    }
  }
  return [...seen];
}

function nonEmpty(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

/** Later sources win field by field, but never with an empty value. */
export function mergeProfiles(
  ...sources: Array<ReadonlyMap<string, ReactorProfile> | undefined>
): Map<string, ReactorProfile> {
  const out = new Map<string, ReactorProfile>();
  for (const source of sources) {
    if (!source) {
      continue;
    }
    for (const [id, profile] of source) {
      const prev = out.get(id) ?? {};
      out.set(id, {
        username: nonEmpty(profile.username) ?? prev.username ?? null,
        preferred_name: nonEmpty(profile.preferred_name) ?? prev.preferred_name ?? null,
        avatar_url:
          profile.avatar_url !== undefined ? profile.avatar_url : prev.avatar_url,
      });
    }
  }
  return out;
}

function reactor(
  userId: string,
  selfId: string | null | undefined,
  profiles: ReadonlyMap<string, ReactorProfile>,
): Reactor {
  const profile = profiles.get(userId);
  const username = nonEmpty(profile?.username);
  const preferred = nonEmpty(profile?.preferred_name);
  const name = preferred ?? username;
  return {
    userId,
    isSelf: !!selfId && userId === selfId,
    name,
    handle: preferred && username && preferred !== username ? username : null,
    avatarKey: profile ? profile.avatar_url : undefined,
    emojis: [],
  };
}

/**
 * Tabs in the order the pills sit on the message, people in reaction order.
 * An All tab leads when there is more than one emoji (Discord's order).
 */
export function buildReactionBreakdown(
  reactions: readonly ReactionInput[],
  selfId: string | null | undefined,
  profiles: ReadonlyMap<string, ReactorProfile>,
): ReactionTab[] {
  const everyone = new Map<string, Reactor>();
  const tabs: ReactionTab[] = [];
  for (const reaction of reactions) {
    const people: Reactor[] = [];
    const inTab = new Set<string>();
    for (const id of reaction.user_ids) {
      // One person per tab, whatever the wire says.
      if (inTab.has(id)) {
        continue;
      }
      inTab.add(id);
      let person = everyone.get(id);
      if (!person) {
        person = reactor(id, selfId, profiles);
        everyone.set(id, person);
      }
      person.emojis.push(reaction.emoji);
      people.push(person);
    }
    if (people.length > 0) {
      tabs.push({ key: reaction.emoji, emoji: reaction.emoji, count: people.length, people });
    }
  }
  if (tabs.length > 1) {
    tabs.unshift({
      key: ALL_TAB_KEY,
      emoji: null,
      count: tabs.reduce((sum, tab) => sum + tab.count, 0),
      people: [...everyone.values()],
    });
  }
  return tabs;
}

/** The tab to show: the wanted one while it exists, else the first emoji's. */
export function selectTab(
  tabs: readonly ReactionTab[],
  wanted: string | null | undefined,
): ReactionTab | null {
  if (tabs.length === 0) {
    return null;
  }
  const match = wanted ? tabs.find((tab) => tab.key === wanted) : undefined;
  if (match) {
    return match;
  }
  return tabs.find((tab) => tab.emoji !== null) ?? tabs[0];
}

/** Up to `max` names for a one-line summary; the rest are counted, not named. */
export function summarizeNames(
  people: readonly Reactor[],
  label: (person: Reactor) => string,
  max = 3,
): { names: string[]; others: number } {
  return {
    names: people.slice(0, max).map(label),
    others: Math.max(0, people.length - max),
  };
}
