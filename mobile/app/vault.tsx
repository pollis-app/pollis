// The Vault (#1316): a chat with yourself, synced to every enrolled device; port of desktop's Vault.tsx.

import { useCallback, useMemo, useState } from "react";
import { FlatList, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import * as ImagePicker from "expo-image-picker";
import { observer } from "mobx-react-lite";
import { Screen, Header, Field, Chip, Button } from "../components/ui";
import { Icon } from "../components/icons";
import { Composer } from "../components/chat/Composer";
import { EditBar } from "../components/chat/EditBar";
import { DaySeparator } from "../components/chat/DaySeparator";
import { afterSheetClose } from "../components/chat/SheetOverlay";
import { dayKey, dayLabel, timeLabel } from "../components/chat/dates";
import { VaultEntryRow } from "../components/vault/VaultEntryRow";
import { VaultEntrySheet } from "../components/vault/VaultEntrySheet";
import { VaultDeleteBar } from "../components/vault/VaultDeleteBar";
import { VaultMediaGrid } from "../components/vault/VaultMediaGrid";
import {
  useDeleteVaultMessage,
  useEditVaultMessage,
  useSendVaultMessage,
  useSetVaultMessagePinned,
  useVaultMessages,
  type VaultMessage,
} from "../hooks/queries";
import { parseContent } from "../lib/messageContent";
import {
  editedVaultContent,
  filterVaultEntries,
  isVaultEntryEdited,
  vaultAttachments,
  vaultSaveFailure,
  type VaultSaveFailure,
} from "../lib/vault";
import { outgoingText } from "../lib/messageText";
import type { PickedAttachment } from "../lib/attachments";
import { pickedFromAssets } from "../lib/composerAttachments";
import { haptic } from "../lib/haptics";
import { appStore } from "../stores/appStore";
import { semantic, type as ty, space } from "../theme/tokens";
import type { MessageAttachment } from "../types";
import { useTheme } from "../components/theme";

type VaultView = "chat" | "media";

const SAVE_FAILED_COPY: Record<VaultSaveFailure, string> = {
  offline: "mobile:vault.saveFailedOffline",
  tooLarge: "mobile:vault.saveFailedTooLarge",
  other: "mobile:vault.saveFailed",
};

type VaultListItem =
  | { type: "sep"; key: string; label: string }
  | { type: "entry"; key: string; entry: VaultMessage };

function entryTime(entry: VaultMessage): number {
  return new Date(entry.created_at).getTime();
}

function VaultScreen() {
  const { accentHex } = useTheme();
  const { t } = useTranslation("vault");
  const router = useRouter();
  const [view, setView] = useState<VaultView>("chat");
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState("");
  const [pendingAttachments, setPendingAttachments] = useState<PickedAttachment[]>([]);
  const [actionTarget, setActionTarget] = useState<VaultMessage | null>(null);
  const [editTarget, setEditTarget] = useState<VaultMessage | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);
  const signedIn = !!appStore.currentUser;

  const { data: entries = [], isLoading, isError } = useVaultMessages();
  const send = useSendVaultMessage();
  const edit = useEditVaultMessage();
  const remove = useDeleteVaultMessage();
  const pin = useSetVaultMessagePinned();

  const visible = useMemo(() => filterVaultEntries(entries, query), [entries, query]);
  const allAttachments = useMemo(() => vaultAttachments(entries), [entries]);

  // Chronological with day separators, then reversed for the inverted list.
  const items = useMemo(() => {
    const out: VaultListItem[] = [];
    let lastKey = "";
    for (const entry of visible) {
      const ts = entryTime(entry);
      const k = dayKey(ts);
      if (k !== lastKey) {
        out.push({ type: "sep", key: `sep-${k}`, label: dayLabel(ts) });
        lastKey = k;
      }
      out.push({ type: "entry", key: entry.id, entry });
    }
    return out.reverse();
  }, [visible]);

  const openAttachment = useCallback(
    (attachment: MessageAttachment) => {
      router.push({
        pathname: "/media",
        params: { source: "vault", attachmentId: attachment.id },
      });
    },
    [router],
  );

  const onSend = () => {
    const text = outgoingText(draft);
    const attachments = pendingAttachments;
    if ((!text && attachments.length === 0) || send.isPending) {
      return;
    }
    setDraft("");
    setPendingAttachments([]);
    send.mutate(
      { text, attachments },
      {
        // A failed save puts the draft back unless another one was started.
        onError: () => {
          setDraft((current) => (current ? current : text));
          setPendingAttachments((current) => (current.length > 0 ? current : attachments));
        },
      },
    );
  };

  const onAttach = useCallback(async () => {
    const result = await ImagePicker.launchImageLibraryAsync({
      mediaTypes: ["images", "videos"],
      allowsMultipleSelection: true,
      quality: 1,
    });
    if (result.canceled) {
      return;
    }
    const picked = pickedFromAssets(result.assets, Date.now());
    setPendingAttachments((prev) => [...prev, ...picked]);
  }, []);

  const startEdit = useCallback((entry: VaultMessage) => {
    setPendingDeleteId(null);
    setEditDraft(parseContent(entry.content, "vault").text);
    setEditTarget(entry);
  }, []);

  const armDelete = useCallback((entry: VaultMessage) => {
    setEditTarget(null);
    haptic("confirmArm");
    setPendingDeleteId(entry.id);
  }, []);

  const togglePin = useCallback(
    (entry: VaultMessage) => {
      pin.mutate({ id: entry.id, pinned: !entry.pinned });
    },
    [pin],
  );

  const onSaveEdit = () => {
    if (!editTarget) {
      return;
    }
    const newContent = editedVaultContent(editTarget.content, editDraft);
    if (!newContent) {
      return;
    }
    edit.mutate(
      { id: editTarget.id, newContent },
      {
        onSuccess: () => {
          setEditTarget(null);
          setEditDraft("");
        },
      },
    );
  };

  const onConfirmDelete = () => {
    if (!pendingDeleteId) {
      return;
    }
    remove.mutate(pendingDeleteId, {
      onSuccess: () => setPendingDeleteId(null),
    });
  };

  const renderItem = useCallback(
    ({ item }: { item: VaultListItem }) => {
      if (item.type === "sep") {
        return <DaySeparator label={item.label} />;
      }
      const entry = item.entry;
      const parsed = parseContent(entry.content, "vault");
      return (
        <VaultEntryRow
          entryId={entry.id}
          time={timeLabel(entryTime(entry))}
          text={parsed.text}
          attachments={parsed.attachments}
          pinned={entry.pinned}
          edited={isVaultEntryEdited(entry)}
          onOpenActions={() => setActionTarget(entry)}
          onTogglePin={() => togglePin(entry)}
          onEdit={() => startEdit(entry)}
          onDelete={() => armDelete(entry)}
          onOpenAttachment={openAttachment}
        />
      );
    },
    [togglePin, startEdit, armDelete, openAttachment],
  );

  const editHasFiles = editTarget
    ? parseContent(editTarget.content, "vault").attachments.length > 0
    : false;
  const actionParsed = actionTarget ? parseContent(actionTarget.content, "vault") : null;

  return (
    <Screen testID="screen-vault" wide>
      <Header title={t("title")} titleIcon={<Icon.vault size={17} color={semantic.dim} />} />
      <View
        style={{
          flexDirection: "row",
          gap: space.sm,
          paddingHorizontal: space.lg,
          paddingTop: space.xs,
        }}
      >
        {/* Touch-down (#1529). */}
        <Chip
          testID="chip-vault-view-chat"
          pressIn
          selected={view === "chat"}
          onPress={() => setView("chat")}
          leading={
            <Icon.messageSquare
              size={16}
              color={view === "chat" ? semantic.onAccent : semantic.accent}
            />
          }
        >
          {t("viewChat")}
        </Chip>
        <Chip
          testID="chip-vault-view-media"
          pressIn
          selected={view === "media"}
          onPress={() => setView("media")}
          leading={
            <Icon.images
              size={16}
              color={view === "media" ? semantic.onAccent : semantic.accent}
            />
          }
        >
          {t("viewMedia")}
        </Chip>
      </View>

      {view === "media" ? (
        <View style={{ flex: 1, paddingTop: space.sm }}>
          <VaultMediaGrid attachments={allAttachments} onOpen={openAttachment} />
        </View>
      ) : (
        <>
          <View style={{ paddingHorizontal: space.lg, paddingVertical: space.sm }}>
            <Field
              testID="input-vault-search"
              accessibilityLabel={t("searchPlaceholder")}
              placeholder={t("searchPlaceholder")}
              value={query}
              onChangeText={setQuery}
              returnKeyType="search"
              autoCorrect={false}
              icon={<Icon.search size={18} color={semantic.muted} />}
            />
          </View>
          {isError ? (
            <Text
              accessibilityRole="alert"
              style={[ty.secondary, { color: semantic.danger, paddingHorizontal: space.lg }]}
            >
              {t("mobile:vault.loadFailed")}
            </Text>
          ) : null}
          {!isLoading && !isError && items.length === 0 ? (
            <Text
              testID="vault-empty"
              style={[
                ty.secondary,
                { textAlign: "center", paddingHorizontal: space.xxl, paddingTop: space.xxl },
              ]}
            >
              {query.trim() ? t("searchEmpty") : t("empty")}
            </Text>
          ) : null}
          <FlatList
            testID="list-vault"
            inverted
            data={items}
            keyExtractor={(item) => item.key}
            renderItem={renderItem}
            extraData={accentHex}
            style={{ flex: 1 }}
            keyboardShouldPersistTaps="handled"
            // Anchor a short vault to the top (the chat's rule).
            contentContainerStyle={{
              paddingTop: 4,
              paddingBottom: 12,
              flexGrow: 1,
              justifyContent: "flex-end",
            }}
          />
          {send.isError ? (
            <View
              style={{
                flexDirection: "row",
                alignItems: "center",
                gap: space.sm,
                paddingHorizontal: space.lg,
                paddingVertical: 4,
              }}
            >
              <Text
                accessibilityRole="alert"
                testID="text-vault-save-failed"
                style={[ty.meta, { color: semantic.danger, flex: 1 }]}
              >
                {t(SAVE_FAILED_COPY[vaultSaveFailure(send.error)])}
              </Text>
              <Button variant="subtle" onPress={onSend} testID="btn-vault-retry">
                {t("mobile:vault.retry")}
              </Button>
            </View>
          ) : null}
          {editTarget ? (
            <EditBar
              draft={editDraft}
              onChangeDraft={setEditDraft}
              onCancel={() => {
                setEditTarget(null);
                setEditDraft("");
              }}
              onSave={onSaveEdit}
              savePending={edit.isPending}
              canSaveEmpty={editHasFiles}
            />
          ) : pendingDeleteId ? (
            <VaultDeleteBar
              onCancel={() => setPendingDeleteId(null)}
              onConfirm={onConfirmDelete}
              pending={remove.isPending}
            />
          ) : (
            <Composer
              draft={draft}
              onChangeDraft={setDraft}
              onSend={onSend}
              sendPending={send.isPending}
              editable={signedIn}
              placeholder={t("placeholder")}
              onAttach={() => void onAttach()}
              pendingAttachments={pendingAttachments}
              onRemoveAttachment={(id) =>
                setPendingAttachments((prev) => prev.filter((a) => a.id !== id))
              }
              onChangeAttachment={(id, patch) =>
                setPendingAttachments((prev) => prev.map((a) => (a.id === id ? { ...a, ...patch } : a)))
              }
              canSendEmptyText={pendingAttachments.length > 0}
            />
          )}
        </>
      )}

      {actionTarget && actionParsed ? (
        <VaultEntrySheet
          quote={actionParsed.text || actionParsed.attachments.map((a) => a.filename).join(", ")}
          quoteTime={`${dayLabel(entryTime(actionTarget))} · ${timeLabel(entryTime(actionTarget))}`}
          pinned={actionTarget.pinned}
          onTogglePin={() => {
            togglePin(actionTarget);
            setActionTarget(null);
          }}
          onEdit={() => {
            const entry = actionTarget;
            setActionTarget(null);
            // Only once the sheet's Modal is gone.
            afterSheetClose(() => startEdit(entry));
          }}
          onDelete={() => {
            const entry = actionTarget;
            setActionTarget(null);
            afterSheetClose(() => armDelete(entry));
          }}
          onClose={() => setActionTarget(null)}
        />
      ) : null}
    </Screen>
  );
}

export default observer(VaultScreen);
