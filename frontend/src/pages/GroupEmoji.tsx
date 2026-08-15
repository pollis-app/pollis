import React, { useState } from "react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { Trash2 } from "lucide-react";
import { appStore } from "../stores/appStore";
import { dialogOpen } from "../bridge";
import { errorMessage } from "../utils/errorMessage";
import {
  useGroupEmoji,
  useRemoveGroupEmoji,
  useUploadGroupEmoji,
} from "../hooks/queries/useEmoji";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";
import { CustomEmojiImage } from "../components/Emoji/CustomEmojiImage";

interface GroupEmojiProps {
  groupId: string;
}

// Mirrors `shortcode_is_valid` in pollis-core and the DS. Client-side it is a
// courtesy — the DS is authoritative — so it may only ever be stricter.
const SHORTCODE_RE = /^[a-z0-9_]{2,32}$/;

/** Human-readable size for the per-emoji storage column. */
function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  return `${(bytes / 1024).toFixed(1)} KB`;
}

/**
 * A group's custom emoji: add, list, remove.
 *
 * The page states the storage model plainly rather than hiding it, because
 * "where did my upload go and how big is it now" is exactly the question a
 * shrink-on-upload feature has to be able to answer. Every image is re-encoded
 * on the Rust side to under 48 KB, and identical images across groups share one
 * stored object — so the size shown is what is actually stored, not what was
 * uploaded.
 *
 * There is deliberately **no per-group limit** (#848). What is bounded is the
 * per-person total, which is what an attacker would have to pay.
 */
export const GroupEmoji: React.FC<GroupEmojiProps> = observer(({ groupId }) => {
  const { t } = useTranslation("emoji");
  const { currentUser } = appStore;
  const { data: emoji = [], isLoading } = useGroupEmoji(groupId);
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const uploadEmoji = useUploadGroupEmoji();
  const removeEmoji = useRemoveGroupEmoji();

  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const isAdmin = group?.current_user_role === "admin";

  const [shortcode, setShortcode] = useState("");
  const [error, setError] = useState<string | null>(null);

  const handleAdd = async () => {
    setError(null);
    const trimmed = shortcode.trim().toLowerCase();
    if (!SHORTCODE_RE.test(trimmed)) {
      setError(t("manage.shortcodeInvalid"));
      return;
    }
    if (emoji.some((e) => e.shortcode === trimmed)) {
      setError(t("manage.shortcodeTaken", { shortcode: trimmed }));
      return;
    }

    // A file PATH, never bytes: the Rust side reads, re-encodes and uploads it,
    // so a multi-megabyte source never crosses the JSON IPC.
    const picked = await dialogOpen({
      multiple: false,
      title: t("manage.filePickerTitle"),
      filters: [
        {
          name: t("manage.imageFilterName"),
          extensions: ["png", "jpg", "jpeg", "gif", "webp"],
        },
      ],
    });
    const path = Array.isArray(picked) ? picked[0] : picked;
    if (!path) {
      return;
    }

    try {
      await uploadEmoji.mutateAsync({ groupId, shortcode: trimmed, path });
      setShortcode("");
    } catch (err) {
      setError(errorMessage(err, t("manage.addFailed")));
    }
  };

  const handleRemove = async (code: string) => {
    setError(null);
    try {
      await removeEmoji.mutateAsync({ groupId, shortcode: code });
    } catch (err) {
      setError(errorMessage(err, t("manage.removeFailed")));
    }
  };

  if (!currentUser) {
    return (
      <div data-testid="group-emoji-no-user" className="flex items-center justify-center flex-1 bg-bg">
        <p className="text-xs font-mono text-muted">{t("manage.signInRequired")}</p>
      </div>
    );
  }

  const totalBytes = emoji.reduce((sum, e) => sum + e.size_bytes, 0);

  return (
    <div data-testid="group-emoji-page" className="flex-1 flex flex-col overflow-auto bg-bg">
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <div className="w-full max-w-xl flex flex-col gap-5">
          <div className="flex items-end gap-2">
            <TextInput
              label={t("manage.shortcodeLabel")}
              value={shortcode}
              onChange={(value) => setShortcode(value.toLowerCase())}
              placeholder={t("manage.shortcodePlaceholder")}
              description={t("manage.shortcodeDescription")}
              disabled={uploadEmoji.isPending}
              data-testid="group-emoji-shortcode"
              id="group-emoji-shortcode"
            />
            <Button
              onClick={handleAdd}
              isLoading={uploadEmoji.isPending}
              loadingText={t("manage.shrinking")}
              data-testid="group-emoji-add"
            >
              {t("manage.chooseImage")}
            </Button>
          </div>

          {error && (
            <p data-testid="group-emoji-error" className="text-xs font-mono text-danger" role="alert">
              {error}
            </p>
          )}

          <p className="text-xs font-mono text-muted">
            {t("manage.storageNote")}
          </p>

          <div className="flex items-center justify-between">
            <span className="section-label px-0">
              {t("manage.stored", {
                count: emoji.length,
                size: formatBytes(totalBytes),
              })}
            </span>
          </div>

          {isLoading && (
            <p data-testid="group-emoji-loading" className="text-xs font-mono text-muted">
              {t("common:states.loading")}
            </p>
          )}

          {!isLoading && emoji.length === 0 && (
            <p data-testid="group-emoji-empty" className="text-xs font-mono text-muted">
              {t("manage.empty")}
            </p>
          )}

          <ul className="flex flex-col gap-1">
            {emoji.map((item) => {
              const canRemove = isAdmin || item.created_by === currentUser.id;
              return (
                <li
                  key={item.shortcode}
                  data-testid="group-emoji-row"
                  data-shortcode={item.shortcode}
                  className="flex items-center gap-3 px-2 py-1.5 panel-raised"
                >
                  <CustomEmojiImage
                    contentHash={item.content_hash}
                    shortcode={item.shortcode}
                    sizeRem={2}
                  />
                  <span className="flex-1 min-w-0 truncate text-sm font-mono text-fg">
                    :{item.shortcode}:
                  </span>
                  <span className="text-xs font-mono text-muted shrink-0">
                    {item.animated
                      ? t("manage.sizeAnimated", { size: formatBytes(item.size_bytes) })
                      : formatBytes(item.size_bytes)}
                  </span>
                  {canRemove && (
                    <button
                      type="button"
                      onClick={() => handleRemove(item.shortcode)}
                      className="icon-btn-sm shrink-0"
                      aria-label={t("manage.remove", { shortcode: item.shortcode })}
                      data-testid={`group-emoji-remove-${item.shortcode}`}
                    >
                      <Trash2 size={14} className="size-[0.933rem]" />
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        </div>
      </div>
    </div>
  );
});
