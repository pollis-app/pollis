import { useObserver } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useGroupCommands } from "./useBots";
import type { GroupCommand } from "../../utils/slashCommands";

export interface CommandCandidates {
  groupId: string | null;
  channelId: string | null;
  /** Every command of the group's Apps: a `request` one is never posted, in any channel. */
  all: GroupCommand[];
}

/** The composer's slash commands (#2018): only in a group channel, never in a DM. */
export function useCommandCandidates(): CommandCandidates {
  const groupId = useObserver(() => appStore.chatGroupId);
  const channelId = useObserver(() => appStore.selectedChannelId);
  const inChannel = !!channelId && !!groupId;
  const commands = useGroupCommands(inChannel ? groupId : null);
  return {
    groupId: inChannel ? groupId : null,
    channelId: inChannel ? channelId : null,
    all: inChannel ? (commands.data ?? []) : [],
  };
}
