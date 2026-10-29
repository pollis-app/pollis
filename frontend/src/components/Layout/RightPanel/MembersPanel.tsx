import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import { presenceStore } from "../../../stores/presenceStore";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { useGroupMembers } from "../../../hooks/queries/useGroups";
import { useMessages, useDMConversations } from "../../../hooks/queries/useMessages";
import { MemberRow } from "./MemberRow";
import { MediaGrid } from "./MediaGrid";
import type { MessageAttachment } from "../../../types";

/** Grid cap: each tile fetches its own bytes, so unbounded would fan out on open. */
const MEDIA_LIMIT = 30;

/** Pictures and videos, the split `MessageItem`'s thumb strip makes. */
const isVisualMedia = (contentType: string): boolean =>
  contentType.startsWith("image/") || contentType.startsWith("video/");

interface MembersPanelProps {
  groupId: string | null;
  channelId: string | null;
  conversationId: string | null;
}

interface Person {
  userId: string;
  label: string;
  avatarKey: string | null;
  isAdmin: boolean;
}

/**
 * Members + shared media for the active conversation (#824). A DM's two
 * participants come from the conversation record; with no conversation the
 * panel shows the online roster and no media.
 */
export const MembersPanel: React.FC<MembersPanelProps> = observer(
  ({ groupId, channelId, conversationId }) => {
    const { t } = useTranslation("nav");
    const { data: groupMembers = [] } = useGroupMembers(groupId);
    const { messages } = useMessages(channelId, conversationId);
    const isTerminal = useSkin() === "terminal";
    const currentUser = appStore.currentUser;
    // React Query, not the store: `appStore.dmConversations` is never written (#906).
    const { data: dmConversations = [] } = useDMConversations();
    const hasContext = Boolean(groupId || channelId || conversationId);

    // Online first, then alphabetical. Reading `presenceStore.isOnline` inside
    // the observer keeps the ordering live.
    const people = useMemo<Person[]>(() => {
      if (!hasContext) {
        // No conversation: everyone visible online, named from the DM list
        // (the raw id for someone we share no DM with).
        const known = new Map<string, { label: string; avatarKey: string | null }>();
        for (const dm of dmConversations) {
          if (dm.user2_id) {
            known.set(dm.user2_id, {
              label: dm.user2_identifier,
              avatarKey: dm.user2_avatar_url ?? null,
            });
          }
        }
        const self: Person[] = currentUser
          ? [
              {
                userId: currentUser.id,
                label: t("members.you"),
                avatarKey: null,
                isAdmin: false,
              },
            ]
          : [];
        const others = Object.keys(presenceStore.byUser)
          .filter((userId) => userId !== currentUser?.id)
          .map((userId) => ({
            userId,
            label: known.get(userId)?.label ?? userId,
            avatarKey: known.get(userId)?.avatarKey ?? null,
            isAdmin: false,
          }))
          .sort((a, b) => a.label.localeCompare(b.label));
        return [...self, ...others];
      }

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
                label: t("members.you"),
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
      t,
      hasContext,
      conversationId,
      dmConversations,
      currentUser,
      groupMembers,
      // `byUser` is replaced, not mutated, so identity is a sound dependency.
      presenceStore.byUser,
    ]);

    const attachments = useMemo(() => {
      const seen = new Set<string>();
      const out: MessageAttachment[] = [];
      for (let i = messages.length - 1; i >= 0 && out.length < MEDIA_LIMIT; i--) {
        const message = messages[i];
        if (message.deleted_at) {
          continue;
        }
        for (const attachment of message.attachments ?? []) {
          if (!isVisualMedia(attachment.content_type)) {
            continue;
          }
          if (out.length >= MEDIA_LIMIT || seen.has(attachment.id)) {
            continue;
          }
          seen.add(attachment.id);
          out.push(attachment);
        }
      }
      return out;
    }, [messages]);

    const rootClass = isTerminal
      ? "flex h-full flex-col overflow-y-auto"
      : "flex h-full flex-col gap-4 overflow-y-auto py-3";
    const emptyClass = isTerminal ? "px-2.5 py-1 text-xs text-muted" : "px-2 text-xs text-muted";

    return (
      <div className={rootClass}>
        <section className={isTerminal ? "flex flex-col" : "flex flex-col gap-1"}>
          <SectionHeader
            label={
              hasContext
                ? t("members.count", { count: people.length })
                : t("members.onlineCount", { count: people.length })
            }
            isTerminal={isTerminal}
          />
          {people.length === 0 ? (
            <p className={emptyClass}>
              {hasContext ? t("members.empty") : t("members.noneOnline")}
            </p>
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

        {hasContext && (
          <section
            className={isTerminal ? "flex flex-col gap-1 pb-2" : "flex flex-col gap-2"}
          >
            <SectionHeader label={t("media.heading")} isTerminal={isTerminal} bordered />
            <MediaGrid attachments={attachments} />
          </section>
        )}
      </div>
    );
  },
);

interface SectionHeaderProps {
  label: string;
  isTerminal: boolean;
  /** Hairline above the header too (every section after the first). */
  bordered?: boolean;
}

/** Section heading; terminal borrows the left sidebar's `SectionHeader` chrome. */
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
