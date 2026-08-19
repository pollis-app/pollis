import React, { useCallback, useEffect, useState } from "react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { Trash2 } from "lucide-react";
import { appStore } from "../stores/appStore";
import { readFile } from "../bridge";
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
import { EmojiDropZone } from "../components/Emoji/EmojiDropZone";
import {
  EMOJI_MAX_INPUT_BYTES,
  SHORTCODE_RE,
  emojiMimeType,
  fileName,
  shortcodeFromFileName,
} from "../components/Emoji/emojiShortcode";
import { EmptyState } from "../components/ui/EmptyState";

interface GroupEmojiProps {
  groupId: string;
}

/** The image chosen but not yet uploaded, plus what can be shown about it. */
interface PickedImage {
  /** Native path — the only thing the upload command is given. */
  path: string;
  name: string;
  /** Blob URL for the local preview; null while it loads or if it failed. */
  previewUrl: string | null;
  /** SOURCE size. The stored size is only known after the re-encode. */
  sizeBytes: number | null;
}

/**
 * Human-readable size for the per-emoji storage column, and for the source
 * file's size before the re-encode has run — which is the only place the MB
 * rung is ever reached, since a stored emoji is capped at 48 KB.
 */
function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * A group's custom emoji: add, list, remove.
 *
 * Adding follows Discord's order — **image first, name derived**. Drop a file
 * anywhere on the window (or click the zone to browse), and the shortcode is
 * auto-filled from the filename, sanitised to `[a-z0-9_]`, for the user to
 * edit. Typing the name first still works and is never clobbered: an
 * auto-filled name is only written into a field the user has not touched.
 * Everything below the zone validates live, so the add button is disabled
 * rather than the failure being discovered by pressing it.
 *
 * The page states the storage model plainly rather than hiding it, because
 * "where did my upload go and how big is it now" is exactly the question a
 * shrink-on-upload feature has to be able to answer. Every image is re-encoded
 * on the Rust side to under 48 KB, and identical images across groups share one
 * stored object — so the size shown in the list is what is actually stored, not
 * what was uploaded. Before upload only the SOURCE size is knowable, and it is
 * labelled as such: the re-encode has not run yet.
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
  // Whether the user has typed in the field. Gates the auto-fill so choosing an
  // image never overwrites a name they wrote themselves.
  const [shortcodeEdited, setShortcodeEdited] = useState(false);
  const [picked, setPicked] = useState<PickedImage | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Release the previous preview when the choice changes, and on unmount.
  const previewUrl = picked?.previewUrl ?? null;
  useEffect(() => {
    return () => {
      if (previewUrl) {
        URL.revokeObjectURL(previewUrl);
      }
    };
  }, [previewUrl]);

  const handlePick = useCallback(
    async (path: string) => {
      setError(null);
      setShortcode((current) => {
        if (shortcodeEdited && current.trim().length > 0) {
          return current;
        }
        return shortcodeFromFileName(path);
      });
      setPicked({ path, name: fileName(path), previewUrl: null, sizeBytes: null });

      // The preview is a courtesy — the upload reads the path itself, so a
      // failure here costs a thumbnail, not the ability to add the emoji.
      let bytes: Uint8Array<ArrayBuffer>;
      try {
        bytes = await readFile(path);
      } catch {
        return;
      }
      if (bytes.byteLength > EMOJI_MAX_INPUT_BYTES) {
        setPicked(null);
        setError(
          t("manage.fileTooLarge", { size: formatBytes(EMOJI_MAX_INPUT_BYTES) }),
        );
        return;
      }
      const url = URL.createObjectURL(
        new Blob([bytes], { type: emojiMimeType(path) }),
      );
      setPicked((current) => {
        // A second pick may have landed while this one was being read.
        if (!current || current.path !== path) {
          URL.revokeObjectURL(url);
          return current;
        }
        return { ...current, previewUrl: url, sizeBytes: bytes.byteLength };
      });
    },
    [shortcodeEdited, t],
  );

  const handleReject = useCallback(
    (path: string) => {
      setError(t("manage.unsupportedFile", { name: fileName(path) }));
    },
    [t],
  );

  const trimmed = shortcode.trim().toLowerCase();
  const isTaken = emoji.some((e) => e.shortcode === trimmed);

  let shortcodeError: string | null = null;
  if (trimmed.length === 0) {
    // Only once there is an image waiting on it — an empty field on an empty
    // form is not an error yet.
    if (picked) {
      shortcodeError = t("manage.shortcodeRequired");
    }
  } else if (!SHORTCODE_RE.test(trimmed)) {
    shortcodeError = t("manage.shortcodeInvalid");
  } else if (isTaken) {
    shortcodeError = t("manage.shortcodeTaken", { shortcode: trimmed });
  }

  const canAdd = !!picked && !shortcodeError && trimmed.length > 0;

  const handleAdd = async () => {
    if (!picked || !canAdd) {
      return;
    }
    setError(null);
    try {
      await uploadEmoji.mutateAsync({ groupId, shortcode: trimmed, path: picked.path });
      setShortcode("");
      setShortcodeEdited(false);
      setPicked(null);
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
      <EmptyState testId="group-emoji-no-user">{t("manage.signInRequired")}</EmptyState>
    );
  }

  const totalBytes = emoji.reduce((sum, e) => sum + e.size_bytes, 0);

  return (
    <div data-testid="group-emoji-page" className="flex-1 flex flex-col overflow-auto bg-bg">
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <div className="w-full max-w-xl flex flex-col gap-5">
          <EmojiDropZone
            onPick={handlePick}
            onReject={handleReject}
            disabled={uploadEmoji.isPending}
          >
            {picked && (
              <span className="flex w-full flex-col items-center gap-2">
                {picked.previewUrl ? (
                  <img
                    src={picked.previewUrl}
                    alt=""
                    className="size-16 object-contain"
                    data-testid="group-emoji-preview"
                  />
                ) : (
                  <span className="size-16 bg-surface-high" aria-hidden="true" />
                )}
                <span className="max-w-full truncate text-sm font-mono text-fg">
                  {picked.name}
                </span>
                <span className="text-xs font-mono text-muted">
                  {picked.sizeBytes === null
                    ? t("manage.reencodeNote")
                    : t("manage.sourceSize", {
                        size: formatBytes(picked.sizeBytes),
                      })}
                </span>
                <span className="text-xs font-mono text-dim">
                  {t("manage.replaceHint")}
                </span>
              </span>
            )}
          </EmojiDropZone>

          <TextInput
            label={t("manage.shortcodeLabel")}
            value={shortcode}
            onChange={(value) => {
              setShortcodeEdited(true);
              setShortcode(value.toLowerCase());
            }}
            placeholder={t("manage.shortcodePlaceholder")}
            description={t("manage.shortcodeDescription")}
            error={shortcodeError ?? undefined}
            disabled={uploadEmoji.isPending}
            data-testid="group-emoji-shortcode"
            id="group-emoji-shortcode"
          />

          <div className="flex items-center justify-between gap-3">
            <span
              className="flex min-w-0 items-center gap-2"
              data-testid="group-emoji-sample"
            >
              {picked?.previewUrl && (
                <img
                  src={picked.previewUrl}
                  alt=""
                  className="size-[1.375rem] shrink-0 object-contain"
                />
              )}
              <span className="truncate text-xs font-mono text-muted">
                {trimmed.length > 0 ? `:${trimmed}:` : t("manage.samplePlaceholder")}
              </span>
            </span>
            <Button
              onClick={handleAdd}
              disabled={!canAdd}
              isLoading={uploadEmoji.isPending}
              loadingText={t("manage.shrinking")}
              data-testid="group-emoji-add"
            >
              {t("manage.add")}
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
