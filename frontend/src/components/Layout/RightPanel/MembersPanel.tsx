import React, { useMemo } from "react";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import { presenceStore } from "../../../stores/presenceStore";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { useGroupMembers } from "../../../hooks/queries/useGroups";
import { useMessages } from "../../../hooks/queries/useMessages";
import { MemberRow } from "./MemberRow";
import { MediaGrid } from "./MediaGrid";
import type { MessageAttachment } from "../../../types";

/**
 * How many attachments the grid renders. Each tile resolves its own bytes
 * through the media server, so an unbounded grid on a media-heavy channel
 * would fan out into hundreds of fetches the moment the panel opens. The cap
 * keeps that bounded; the conversation itself remains the full archive.
 */
const MEDIA_LIMIT = 30;

interface MembersPanelProps {
  groupId: string | null;
  channelId: string | null;
  conversationId: string | null;
}

/**
 * Members + shared media for the active conversation — Discord's member list
 * over Messenger's shared-media grid, which is what #824 asks for.
 *
 * Members come from the group for a channel/group route. A DM has no member
 * table, so its two participants are derived from the conversation record.
 */
export const MembersPanel: React.FC<MembersPanelProps> = observer(
  ({ groupId, channelId, conversationId }) => {
    const { data: groupMembers = [] } = useGroupMembers(groupId);
    const { messages } = useMessages(channelId, conversationId);
    const isTerminal = useSkin() === "terminal";
    const currentUser = appStore.currentUser;
    const dmConversations = appStore.dmConversations;

    // Online first, then alphabetical. Reading `presenceStore.isOnline` here
    // (inside an observer) is what keeps the ordering live as people join and
    // leave — `isOnline` is deliberately not an action for exactly this.
    const people = useMemo(() => {
      if (conversationId) {
        const dm = dmConversations.find((c) => c.id === conversationId);
        if (!dm) {
          return [];
        }
        const peer = dm.user2_id
          ? [
              {
                userId: dm.user2_id,
                label: dm.user2_identifier,
                avatarKey: dm.user2_avatar_url ?? null,
                isAdmin: false,
              },
            ]
          : [];
        const self = currentUser
          ? [
              {
                userId: currentUser.id,
                label: "You",
                avatarKey: null,
                isAdmin: false,
              },
            ]
          : [];
        return [...peer, ...self];
      }

      return [...groupMembers]
        .map((m) => ({
          userId: m.user_id,
          label: m.display_name || m.username || m.user_id,
          avatarKey: m.avatar_url ?? null,
          isAdmin: m.role === "admin",
        }))
        .sort((a, b) => {
          const aOnline = presenceStore.isOnline(a.userId);
          const bOnline = presenceStore.isOnline(b.userId);
          if (aOnline !== bOnline) {
            return aOnline ? -1 : 1;
          }
          return a.label.localeCompare(b.label);
        });
    }, [
      conversationId,
      dmConversations,
      currentUser,
      groupMembers,
      // Re-sort when anyone's presence flips. `byUser` is replaced (not
      // mutated) by the store, so identity is a sound dependency.
      presenceStore.byUser,
    ]);

    const attachments = useMemo(() => {
      const seen = new Set<string>();
      const out: MessageAttachment[] = [];
      // `messages` is oldest-first; walk backwards so the grid leads with the
      // most recent media.
      for (let i = messages.length - 1; i >= 0 && out.length < MEDIA_LIMIT; i--) {
        const message = messages[i];
        if (message.deleted_at) {
          continue;
        }
        for (const attachment of message.attachments ?? []) {
          if (out.length >= MEDIA_LIMIT || seen.has(attachment.id)) {
            continue;
          }
          seen.add(attachment.id);
          out.push(attachment);
        }
      }
      return out;
    }, [messages]);

    // Terminal packs the column the way the left sidebar does — sections butt
    // up against their hairline headers with no outer padding. Refined keeps
    // the airier card rhythm.
    const rootClass = isTerminal
      ? "flex h-full flex-col overflow-y-auto"
      : "flex h-full flex-col gap-4 overflow-y-auto py-3";
    const emptyClass = isTerminal ? "px-2.5 py-1 text-xs text-muted" : "px-2 text-xs text-muted";

    return (
      <div className={rootClass}>
        <section className={isTerminal ? "flex flex-col" : "flex flex-col gap-1"}>
          <SectionHeader label={`Members — ${people.length}`} isTerminal={isTerminal} />
          {people.length === 0 ? (
            <p className={emptyClass}>No members to show.</p>
          ) : (
            people.map((person) => (
              <MemberRow
                key={person.userId}
                userId={person.userId}
                label={person.label}
                avatarKey={person.avatarKey}
                isAdmin={person.isAdmin}
              />
            ))
          )}
        </section>

        <section className={isTerminal ? "flex flex-col gap-1 pb-2" : "flex flex-col gap-2"}>
          <SectionHeader label="Media" isTerminal={isTerminal} bordered />
          <MediaGrid attachments={attachments} />
        </section>
      </div>
    );
  },
);

interface SectionHeaderProps {
  label: string;
  isTerminal: boolean;
  /** Hairline rule above the header as well as below — the left sidebar's
      `bordered` treatment, used on every section after the first. */
  bordered?: boolean;
}

/**
 * Section heading for the panel column. Terminal borrows the left sidebar's
 * `SectionHeader` chrome verbatim — a `h-bar` sticky strip, hairline under it,
 * and a second hairline above every section after the first — so both columns
 * read as the same UI. Refined keeps the lighter free-standing label it
 * already had.
 */
const SectionHeader: React.FC<SectionHeaderProps> = ({
  label,
  isTerminal,
  bordered,
}) => {
  if (!isTerminal) {
    return (
      <h2 className="px-2 text-xs font-medium uppercase tracking-widest text-muted">
        {label}
      </h2>
    );
  }
  const cls = [
    "sticky top-0 z-[1] flex h-bar w-full items-center border-b border-line",
    "bg-surface px-2.5 text-[0.8rem] uppercase tracking-[0.08em] text-muted select-none",
    bordered ? "mt-1 border-t" : "",
  ]
    .filter(Boolean)
    .join(" ");
  return <h2 className={cls}>{label}</h2>;
};
