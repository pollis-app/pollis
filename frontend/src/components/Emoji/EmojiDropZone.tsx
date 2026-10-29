import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ImagePlus } from "lucide-react";
import { dialogOpen } from "../../bridge";
import { dropTargetStore } from "../../stores/dropTargetStore";
import { EMOJI_IMAGE_EXTENSIONS, isSupportedEmojiFile } from "./emojiShortcode";

interface EmojiDropZoneProps {
  onPick: (path: string) => void;
  /** A drop this zone will not take (wrong type). */
  onReject: (path: string) => void;
  disabled?: boolean;
  children?: React.ReactNode;
}

/**
 * The image-first step of the custom-emoji upload (Discord's model): drop a file
 * anywhere on the window, or click to pick.
 *
 * Tauri hands the window a payload of PATHS (Rust reads and re-encodes, so bytes
 * never cross the IPC), so "drag-over" is window-level: the zone lights while a
 * drag is anywhere over the app. Registered as an INLINE drop target so
 * `AppShell`'s drop overlay doesn't cover it.
 */
export const EmojiDropZone: React.FC<EmojiDropZoneProps> = ({
  onPick,
  onReject,
  disabled = false,
  children,
}) => {
  const { t } = useTranslation("emoji");
  const [isDragOver, setIsDragOver] = useState(false);

  useEffect(() => {
    const { registerInline, unregisterInline } = dropTargetStore;
    registerInline();
    return unregisterInline;
  }, []);

  const handlePaths = useCallback(
    (paths: string[]) => {
      const path = paths[0];
      if (disabled || !path) {
        return;
      }
      if (!isSupportedEmojiFile(path)) {
        onReject(path);
        return;
      }
      onPick(path);
    },
    [disabled, onPick, onReject],
  );

  useEffect(() => {
    const handleDrop = (e: Event) => {
      setIsDragOver(false);
      handlePaths((e as CustomEvent<{ paths: string[] }>).detail.paths);
    };
    const handleDragState = (e: Event) => {
      const dragging = (e as CustomEvent<{ dragging: boolean }>).detail.dragging;
      setIsDragOver(dragging && !disabled);
    };
    window.addEventListener("pollis:pathdrop", handleDrop);
    window.addEventListener("pollis:pathdragstate", handleDragState);
    return () => {
      window.removeEventListener("pollis:pathdrop", handleDrop);
      window.removeEventListener("pollis:pathdragstate", handleDragState);
    };
  }, [handlePaths, disabled]);

  const handleClick = useCallback(async () => {
    if (disabled) {
      return;
    }
    // A PATH, never bytes: Rust reads, re-encodes and uploads it.
    const picked = await dialogOpen({
      multiple: false,
      title: t("manage.filePickerTitle"),
      filters: [
        {
          name: t("manage.imageFilterName"),
          extensions: EMOJI_IMAGE_EXTENSIONS,
        },
      ],
    });
    const path = Array.isArray(picked) ? picked[0] : picked;
    if (path) {
      handlePaths([path]);
    }
  }, [disabled, handlePaths, t]);

  const stateClass = disabled
    ? "border-line bg-surface cursor-not-allowed opacity-50"
    : isDragOver
      ? "border-accent bg-active cursor-copy"
      : "border-line bg-surface cursor-pointer hover:border-line-strong hover:bg-hover";

  return (
    <button
      type="button"
      onClick={handleClick}
      disabled={disabled}
      data-testid="group-emoji-dropzone"
      data-dragover={isDragOver ? "true" : "false"}
      className={`w-full flex flex-col items-center justify-center gap-2 px-4 py-6 border-2 border-dashed rounded-panel transition-colors ${stateClass}`}
    >
      {children ?? (
        <>
          <ImagePlus className="size-6 text-muted" aria-hidden="true" />
          <span className="text-sm font-mono text-fg">
            {isDragOver ? t("manage.dropNow") : t("manage.dropPrompt")}
          </span>
          <span className="text-xs font-mono text-muted">
            {t("manage.dropHint", {
              formats: EMOJI_IMAGE_EXTENSIONS.join(", "),
            })}
          </span>
        </>
      )}
    </button>
  );
};
