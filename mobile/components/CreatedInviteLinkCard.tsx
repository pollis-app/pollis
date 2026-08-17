// #847 (mobile) — the one and only view of a freshly minted invite link.
//
// The server stores only `sha256(secret)`, so unlike Slack or Discord — which
// keep the code in plaintext and let you re-open it forever — this link
// genuinely cannot be shown again by anyone, including us. The card says so
// out loud instead of hiding it behind a support question later.
//
// Copy is VERIFIED (#897/#958 semantics): `Clipboard.setStringAsync` resolves
// to a boolean, and a rejected or false write surfaces as a visible failed
// state — a link that is only ever shown once and was never actually copied is
// the worst possible thing to report as a success. We deliberately do NOT
// read the clipboard back to double-check: on iOS 14+ a clipboard read fires
// the system "pasted from Pollis" banner on every tap.

import { useEffect, useRef, useState } from "react";
import { View, Text, Share } from "react-native";
import * as Clipboard from "expo-clipboard";
import { Card, Button } from "./ui";
import { Icon } from "./icons";
import { palette, semantic, type as ty } from "../theme/tokens";
import type { CreatedInviteLink } from "../hooks/queries";

type CopyState = "idle" | "copied" | "failed";

// How long an outcome stays on the button before it returns to idle.
const COPY_FEEDBACK_MS = 2000;

export function CreatedInviteLinkCard({ link }: { link: CreatedInviteLink }) {
  const [copyState, setCopyState] = useState<CopyState>("idle");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Clear the outcome a couple of seconds after it appears, and never leave a
  // timer behind that would set state on an unmounted card.
  useEffect(() => {
    if (copyState === "idle") {
      return;
    }
    timer.current = setTimeout(() => setCopyState("idle"), COPY_FEEDBACK_MS);
    return () => {
      if (timer.current) {
        clearTimeout(timer.current);
      }
    };
  }, [copyState]);

  const onCopy = async () => {
    // Back to idle first so a second tap on an already-failed button is a
    // visible state change, and so the feedback timer restarts.
    setCopyState("idle");
    const copied = await Clipboard.setStringAsync(link.url).catch(() => false);
    setCopyState(copied ? "copied" : "failed");
  };

  const onShare = () => {
    // Native share sheet. Failures (user dismissed) are not an error state.
    Share.share({ message: link.url }).catch(() => {});
  };

  const bounds: string[] = [];
  if (link.max_uses != null) {
    bounds.push(link.max_uses === 1 ? "1 use" : `${link.max_uses} uses`);
  }
  if (link.expires_at) {
    bounds.push(`expires ${new Date(link.expires_at).toLocaleString()}`);
  }
  const boundsLabel =
    bounds.length > 0 ? bounds.join(" · ") : "No expiry · unlimited uses";

  return (
    <Card style={{ gap: 10 }}>
      <Text style={[ty.label, { color: semantic.accent }]}>
        LINK CREATED — COPY IT NOW
      </Text>
      <Text
        style={{
          fontFamily: ty.body.fontFamily,
          fontSize: 11,
          color: semantic.mute,
          lineHeight: 16,
        }}
      >
        This is the only time this link can be shown. Nobody — including you —
        can view it again. To share it later, create a new one.
      </Text>
      <View
        style={{
          borderWidth: 1,
          borderColor: semantic.hairStrong,
          backgroundColor: palette.bg,
          paddingVertical: 8,
          paddingHorizontal: 10,
          borderRadius: 3,
        }}
      >
        <Text
          testID="created-invite-link-url"
          selectable
          style={{
            fontFamily: ty.mono.fontFamily,
            fontSize: 11,
            color: semantic.ink,
          }}
        >
          {link.url}
        </Text>
      </View>
      <View style={{ flexDirection: "row", gap: 8 }}>
        <View style={{ flex: 1 }}>
          <Button
            full
            testID="btn-copy-invite-link"
            variant={copyState === "failed" ? "danger" : "primary"}
            onPress={onCopy}
            icon={
              copyState === "copied" ? (
                <Icon.check color={palette.bg} />
              ) : copyState === "failed" ? (
                <Icon.alert color={semantic.danger} />
              ) : (
                <Icon.copy color={palette.bg} />
              )
            }
          >
            {copyState === "copied"
              ? "COPIED"
              : copyState === "failed"
                ? "COPY FAILED"
                : "COPY"}
          </Button>
        </View>
        <View style={{ flex: 1 }}>
          <Button
            full
            testID="btn-share-invite-link"
            onPress={onShare}
            icon={<Icon.share color={semantic.ink} />}
          >
            SHARE
          </Button>
        </View>
      </View>
      <Text
        style={{
          fontFamily: ty.body.fontFamily,
          fontSize: 11,
          color: semantic.mute,
        }}
      >
        {boundsLabel}
      </Text>
    </Card>
  );
}
