// The one screen a build below the published minimum shows (#1611): what happened, where to
// update, and Retry. Rendered by the root layout IN PLACE of the navigator, so nothing else is
// reachable from here.
import { useEffect, useState } from "react";
import { BackHandler, Image, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Screen, Txt, Button } from "./ui";
import { Icon } from "./icons";
import { useThemeRerender } from "./theme";
import { space } from "../theme/tokens";
import { getBuildInfo, openUpdateDestination } from "../lib/buildInfo";
import { bodyText, openLabelKey, versionsLine, type ClientPolicyStatus } from "../lib/clientPolicy";

export function ClientBlockedScreen({
  status,
  onRetry,
}: {
  status: ClientPolicyStatus;
  // Resolves when the re-check has landed (blocked or not).
  onRetry: () => Promise<void>;
}) {
  useThemeRerender();
  const { t } = useTranslation("nav");
  const [checking, setChecking] = useState(false);
  const [openFailed, setOpenFailed] = useState<string | null>(null);
  const destination = getBuildInfo().destination;
  const versions = versionsLine(status);

  // Android back has nowhere to go: the app is unusable until it is updated.
  useEffect(() => {
    const sub = BackHandler.addEventListener("hardwareBackPress", () => true);
    return () => sub.remove();
  }, []);

  const open = async () => {
    setOpenFailed(null);
    const ok = await openUpdateDestination(destination);
    if (!ok) {
      setOpenFailed(destination.fallbackUrl ?? destination.url);
    }
  };

  const retry = async () => {
    // Busy on the same frame; the core bounds the fetch to a few seconds.
    setChecking(true);
    try {
      await onRetry();
    } finally {
      setChecking(false);
    }
  };

  return (
    <Screen testID="screen-client-blocked" centered>
      <View
        style={{
          flex: 1,
          justifyContent: "center",
          paddingHorizontal: space.xxl,
          gap: space.xl,
        }}
      >
        <Image
          source={require("../assets/splash-glyph.png")}
          style={{ width: 72, height: 72, alignSelf: "center" }}
          accessibilityIgnoresInvertColors
        />
        <Txt variant="title" accessibilityRole="header" style={{ textAlign: "center" }}>
          {t("clientBlocked.title")}
        </Txt>
        <Txt variant="body" style={{ textAlign: "center" }} testID="client-blocked-body">
          {bodyText(status, t("clientBlocked.body"))}
        </Txt>
        {versions ? (
          <Txt variant="meta" style={{ textAlign: "center" }} testID="client-blocked-versions">
            {t(versions.key, versions.vars)}
          </Txt>
        ) : null}
        <View style={{ gap: space.md, marginTop: space.lg }}>
          <Button
            variant="primary"
            full
            icon={<Icon.external />}
            onPress={() => void open()}
            testID="btn-client-blocked-open"
          >
            {t(openLabelKey(destination.kind))}
          </Button>
          <Button
            variant="secondary"
            full
            loading={checking}
            onPress={() => void retry()}
            testID="btn-client-blocked-retry"
            accessibilityLabel={checking ? t("clientBlocked.checking") : t("clientBlocked.retry")}
          >
            {checking ? t("clientBlocked.checking") : t("clientBlocked.retry")}
          </Button>
        </View>
        {openFailed ? (
          <Txt
            variant="secondary"
            style={{ textAlign: "center" }}
            accessibilityLiveRegion="polite"
            testID="client-blocked-open-failed"
          >
            {t("clientBlocked.openFailed", { url: openFailed })}
          </Txt>
        ) : null}
      </View>
    </Screen>
  );
}
