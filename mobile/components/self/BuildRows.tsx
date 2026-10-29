// Self -> App: which build this is and where it updates from (#1443). Two
// rows, exported separately because `Group` draws dividers between direct
// children.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ListRow } from "../ui";
import { Icon } from "../icons";
import { semantic } from "../../theme/tokens";
import { activeLocale } from "../../i18n";
import { haptic } from "../../lib/haptics";
import { getBuildInfo, openUpdateDestination } from "../../lib/buildInfo";
import { shortUpdateId, type UpdateDestinationKind } from "../../lib/installChannel";

const GLYPH = 22;

// Proper names, the same in every language.
const PLACE: Record<UpdateDestinationKind, string> = {
  play: "Google Play",
  appstore: "App Store",
  testflight: "TestFlight",
  web: "pollis.com",
};

/** "1.0.16 (1001600)", plus the OTA update it is running, if any. */
export function VersionRow() {
  const { t } = useTranslation("mobile");
  const info = getBuildInfo();
  const value = info.version
    ? info.build
      ? t("mobile:self.hub.versionValue", { version: info.version, build: info.build })
      : info.version
    : "—";
  const sub = info.ota
    ? t("mobile:self.hub.runningUpdate", {
        id: shortUpdateId(info.ota.id),
        date: info.ota.createdAt
          ? info.ota.createdAt.toLocaleDateString(activeLocale(), {
              year: "numeric",
              month: "short",
              day: "numeric",
            })
          : "—",
      })
    : undefined;
  return (
    <ListRow
      testID="row-self-version"
      glyph={<Icon.info size={GLYPH} color={semantic.text} />}
      name={t("mobile:self.hub.version")}
      sub={sub}
      value={value}
    />
  );
}

export function UpdateRow() {
  const { t } = useTranslation("mobile");
  const { destination } = getBuildInfo();
  const place = PLACE[destination.kind];
  const [opening, setOpening] = useState(false);
  const [failed, setFailed] = useState(false);

  const onPress = () => {
    if (opening) {
      return;
    }
    setOpening(true);
    setFailed(false);
    void openUpdateDestination(destination).then((ok) => {
      setOpening(false);
      if (!ok) {
        setFailed(true);
        haptic("error");
      }
    });
  };

  return (
    <ListRow
      testID="row-self-update"
      glyph={<Icon.download size={GLYPH} color={semantic.text} />}
      name={t("mobile:self.hub.update")}
      sub={failed ? t("mobile:self.hub.updateFailed", { place }) : undefined}
      value={place}
      accessibilityHint={t("mobile:self.hub.updateHint", { place })}
      end={<Icon.external size={18} color={semantic.muted} />}
      disabled={opening}
      onPress={onPress}
    />
  );
}
