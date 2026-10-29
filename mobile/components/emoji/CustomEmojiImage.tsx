import { useEffect, useRef, useState } from "react";
import { AppState, Image, Text, View } from "react-native";
import { semantic, fonts } from "../../theme/tokens";
import { forgetEmojiUri, resolveEmojiUri } from "../../lib/emojiCache";
import { emojiRetryDelay, retryOnForeground } from "../../lib/emojiRetry";
import { reportEmojiShown } from "../../lib/emojiRenderState";

/**
 * One custom emoji, inline: the cached `file://` image for its content hash,
 * or the literal `:shortcode:` text on failure, retried (#1562).
 *
 * While unresolved it is a plain View, never a source-less <Image>: on iOS
 * that fires `onError` and latched the fallback (#1562). Core RN <Image>, not
 * expo-image, so it takes part in inline <Text> layout. What it shows is
 * reported to `lib/emojiRenderState` for `EmojiProbe`.
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
