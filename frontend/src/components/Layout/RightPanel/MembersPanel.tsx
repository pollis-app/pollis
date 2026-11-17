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
import { useRightPanel } from "./useRightPanel";
import { ShieldCheck } from "lucide-react";
import type { MessageAttachment } from "../../../types";
import { EmptyState } from "../../ui/EmptyState";

/** Caps per-tile media fetches when the panel opens. */
const MEDIA_LIMIT = 30;

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

/** People + shared media (#824): a DM's members, a group's online members, or else your online DM contacts. */
export const MembersPanel: React.FC<MembersPanelProps> = observer(
  ({ groupId, channelId, conversationId }) => {
    const { t } = useTranslation("nav");
    const { t: tDms } = useTranslation("dms");
    const { setSafety } = useRightPanel();
    const { data: groupMembers = [] } = useGroupMembers(groupId);
    const { messages } = useMessages(channelId, conversationId);
    const isTerminal = useSkin() === "terminal";
    const currentUser = appStore.currentUser;
    // React Query, not `appStore.dmConversations` (never written, #906).
    const { data: dmConversations = [] } = useDMConversations();
    // Only a DM lists everyone; a group and the default list who is online.
    const listsEveryone = Boolean(conversationId);
    // Media needs a message log: a channel or a DM.
    const showMedia = Boolean(channelId || conversationId);

    // Who the panel lists (#824), by the page's context (`panelContext.ts`):
    // - a DM: everyone in it, you included, with safety numbers and media;
    // - a group page or channel: that group's OTHER members who are online;
    // - any other page: the people you have a DM open with who are online.
    // Reading `isOnline` / `byUser` in the observer keeps it live.
    const people = useMemo<Person[]>(() => {
      if (conversationId) {
        const dm = dmConversations.find((c) => c.id === conversationId);
        if (!dm) {
          return [];
        }
        // Every other member: group DMs have several (#1563).
        const peer = (dm.peers ?? []).length > 0
          ? (dm.peers ?? []).map((p) => ({
              userId: p.user_id,
              label: p.username || p.user_id,
              avatarKey: p.avatar_url ?? null,
              isAdmin: false,
            }))
          : dm.user2_id
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

      if (groupId) {
        return groupMembers
          .filter((m) => m.user_id !== currentUser?.id && presenceStore.isOnline(m.user_id))
          .map((m) => ({
            userId: m.user_id,
            label: m.display_name || m.username || m.user_id,
            avatarKey: m.avatar_url ?? null,
            isAdmin: m.role === "admin",
          }))
          .sort((a, b) => a.label.localeCompare(b.label));
      }

      // No conversation: online DM contacts, named from the DM list (never a raw id).
      const contacts = new Map<string, Person>();
      for (const dm of dmConversations) {
        const peers = (dm.peers ?? []).length > 0
          ? (dm.peers ?? []).map((p) => ({ id: p.user_id, label: p.username || p.user_id, avatar: p.avatar_url ?? null }))
          : dm.user2_id
            ? [{ id: dm.user2_id, label: dm.user2_identifier, avatar: dm.user2_avatar_url ?? null }]
            : [];
        for (const p of peers) {
          if (p.id === currentUser?.id || contacts.has(p.id) || !presenceStore.isOnline(p.id)) {
            continue;
          }
          contacts.set(p.id, { userId: p.id, label: p.label, avatarKey: p.avatar, isAdmin: false });
        }
      }
      return [...contacts.values()].sort((a, b) => a.label.localeCompare(b.label));
    }, [
      t,
      groupId,
      conversationId,
      dmConversations,
      currentUser,
      groupMembers,
      // `byUser` is replaced, not mutated, so identity is a sound dep.
      presenceStore.byUser,
    ]);

    const dmPeers = conversationId
      ? people.filter((p) => p.userId !== currentUser?.id)
      : [];

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

    return (
      <div className={rootClass}>
        <section className={isTerminal ? "flex flex-col" : "flex flex-col gap-1"}>
          <SectionHeader
            label={
              listsEveryone
                ? t("members.count", { count: people.length })
                : t("members.onlineCount", { count: people.length })
            }
            isTerminal={isTerminal}
          />
          {people.length === 0 ? (
            <EmptyState background={false} testId="right-panel-members-empty">
              {listsEveryone ? t("members.empty") : t("members.noneOnline")}
            </EmptyState>
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

        {dmPeers.length > 0 && (
          <section
            className={isTerminal ? "flex flex-col" : "flex flex-col gap-1"}
            data-testid="right-panel-safety"
          >
            <SectionHeader
              label={
                dmPeers.length === 1
                  ? tDms("profile.safetyNumber")
                  : tDms("safety.headingMany")
              }
              isTerminal={isTerminal}
              bordered
            />
            {dmPeers.map((p) => (
              <button
                key={p.userId}
                type="button"
                onClick={() => setSafety(p.userId)}
                className={
                  isTerminal
                    ? "flex w-full items-center gap-1.5 px-2.5 py-0.5 text-start text-base text-fg hover:bg-hover"
                    : "flex w-full items-center gap-2 rounded px-2 py-1 text-start text-sm text-fg hover:bg-hover"
                }
                data-testid={`right-panel-verify-safety-${p.userId}`}
              >
                <ShieldCheck size={14} aria-hidden="true" className="shrink-0 text-muted" />
                <bdi className="min-w-0 flex-1 truncate">
                  {dmPeers.length === 1
                    ? tDms("safety.verify")
                    : tDms("safety.title", { name: `@${p.label}` })}
                </bdi>
              </button>
            ))}
          </section>
        )}

        {showMedia && (
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
  /** Hairline above the header too, for every section after the first. */
  bordered?: boolean;
}

/** Terminal borrows the left sidebar's `SectionHeader` chrome. */
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
