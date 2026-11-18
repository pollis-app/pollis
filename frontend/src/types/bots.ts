// Bots (#1182): a bot is a user scoped to one group; its hosts are its devices.
// Mirrors the bot command types in pollis-core (serde `rename_all = "camelCase"`).

export interface BotSummary {
  userId: string;
  groupId: string;
  displayName: string;
  username: string;
  createdBy: string;
  createdAt: string;
  hostCount: number;
}

/** One machine the bot runs on (a `user_device` row under the bot). */
export interface BotHost {
  deviceId: string;
  name: string;
  lastSeenAt: string | null;
  createdAt: string;
}

export interface BotDetail extends BotSummary {
  hosts: BotHost[];
  groupBotCap: number;
}
