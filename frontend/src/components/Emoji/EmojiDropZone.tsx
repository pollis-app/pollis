import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ImagePlus } from "lucide-react";
import { dialogOpen } from "../../bridge";
import { dropTargetStore } from "../../stores/dropTargetStore";
import { EMOJI_IMAGE_EXTENSIONS, isSupportedEmojiFile } from "./emojiShortcode";

interface EmojiDropZoneProps {
  /** Called with the native path of an accepted image. */
  onPick: (path: string) => void;
  /** Called with the path of a drop this zone will not take (wrong type). */
  onReject: (path: string) => void;
  disabled?: boolean;
  /** Preview of the file already chosen; the prompt shows when absent. */
  children?: React.ReactNode;
}

/**
 * The image-first half of the custom-emoji upload (Discord's model): drop a
 * file anywhere on the window, or click to open the OS picker. The shortcode is
 * derived from whatever lands here, so this is the step that comes first.
 *
 * A native drag never reaches the DOM — Tauri intercepts the OS drop and hands
 * the window a payload of PATHS, which is exactly what the upload wants (the
 * Rust side reads and re-encodes the file, so bytes never cross the IPC). The
 * consequence is that "drag-over" is a window-level fact, not a hover: the zone
 * lights up while a drag is anywhere over the app, because it is the only place
 * on the page that can receive one.
 *
 * Registered as an INLINE drop target, so `AppShell`'s full-window drop overlay
 * stays out of the way — it would otherwise cover the affordance being lit up.
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
    // A file PATH, never bytes: the Rust side reads, re-encodes and uploads it,
    // so a multi-megabyte source never crosses the JSON IPC.
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
