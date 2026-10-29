import { useEffect, useRef, useState } from "react";
import { AppState, Image, Text, View } from "react-native";
import { semantic, fonts } from "../../theme/tokens";
import { forgetEmojiUri, resolveEmojiUri } from "../../lib/emojiCache";
import { emojiRetryDelay, retryOnForeground } from "../../lib/emojiRetry";
import { reportEmojiShown } from "../../lib/emojiRenderState";

/**
 * One custom emoji, rendered inline. Resolves the content hash to a cached
 * `file://` image; while unresolved it holds a fixed-size placeholder (no
 * reflow), and on failure it degrades to the literal `:shortcode:` text —
 * which is the entire reason the shortcode travels alongside the hash.
 *
 * The placeholder is a plain View, never an <Image> without a source: on iOS
 * a source-less <Image> fires `onError` ("The file “Pollis” couldn't be
 * opened"), which used to latch the fallback before `get_emoji_path` had even
 * answered, so every custom emoji showed as text on iOS (#1562).
 *
 * A failure is not final (#1562): a failed resolve, or an image that would not
 * draw, retries on `emojiRetryDelay`'s short schedule and then whenever the
 * app returns to the foreground.
 *
 * What it shows (image or fallback) is reported to `lib/emojiRenderState`,
 * which `EmojiProbe` turns into the `emoji-image-<shortcode>` /
 * `emoji-fallback-<shortcode>` testIDs the Maestro tier asserts on.
 *
 * Uses the core RN <Image> (not expo-image) so it participates in inline
 * <Text> layout.
 */
export function CustomEmojiImage({
  shortcode,
  contentHash,
  size = 18,
}: {
  shortcode: string;
  contentHash: string;
  size?: number;
}) {
  const [uri, setUri] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [drawn, setDrawn] = useState(false);
  // Bumped to run the resolve again.
  const [attempt, setAttempt] = useState(0);
  const failures = useRef(0);
  const retryTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearRetry = () => {
    if (retryTimer.current !== null) {
      clearTimeout(retryTimer.current);
      retryTimer.current = null;
    }
  };

  const fail = (why: string) => {
    console.warn(`[emoji] ${contentHash} (:${shortcode}:) ${why}`);
    setFailed(true);
    setUri(null);
    failures.current += 1;
    clearRetry();
    const delay = emojiRetryDelay(failures.current);
    if (delay !== null) {
      retryTimer.current = setTimeout(() => {
        retryTimer.current = null;
        setAttempt((n) => n + 1);
      }, delay);
    }
  };

  // A new emoji starts from a clean slate.
  useEffect(() => {
    failures.current = 0;
    setFailed(false);
    return clearRetry;
  }, [contentHash]);

  useEffect(() => {
    let cancelled = false;
    setUri(null);
    setDrawn(false);
    resolveEmojiUri(contentHash)
      .then((resolved) => {
        if (!cancelled) {
          setUri(resolved);
        }
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          fail(`could not be resolved: ${String(e)}`);
        }
      });
    return () => {
      cancelled = true;
    };
    // `fail` only touches refs and setters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [contentHash, attempt]);

  // Past the timed schedule, coming back to the app tries again.
  useEffect(() => {
    if (!failed) {
      return;
    }
    const sub = AppState.addEventListener("change", (state) => {
      if (state === "active" && retryOnForeground(true, retryTimer.current !== null)) {
        setAttempt((n) => n + 1);
      }
    });
    return () => sub.remove();
  }, [failed]);

  const showing = uri && drawn ? "image" : failed && !uri ? "fallback" : null;
  useEffect(() => reportEmojiShown(shortcode, showing), [shortcode, showing]);

  if (failed && !uri) {
    return (
      <Text
        style={{
          fontFamily: fonts.mono400,
          fontSize: size * 0.7,
          color: semantic.muted,
        }}
      >
        :{shortcode}:
      </Text>
    );
  }

  if (!uri) {
    return (
      <View
        accessibilityLabel={`:${shortcode}:`}
        style={{ width: size, height: size }}
      />
    );
  }

  return (
    <Image
      source={{ uri }}
      onLoad={() => {
        failures.current = 0;
        clearRetry();
        setFailed(false);
        setDrawn(true);
      }}
      onError={(e) => {
        // The file it was given would not draw: re-materialise it next time.
        forgetEmojiUri(contentHash);
        fail(`did not draw from ${uri}: ${e.nativeEvent.error}`);
      }}
      accessibilityLabel={`:${shortcode}:`}
      style={{ width: size, height: size, resizeMode: "contain" }}
    />
  );
}
