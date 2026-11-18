import { useTranslation } from "react-i18next";
import { Tag } from "../Tag";
import { Icon } from "../icons";
import { semantic } from "../../theme/tokens";

export type AppMarkKind = "development" | "noEgress" | "autoUpdate";

const LABEL_KEYS: Record<AppMarkKind, string> = {
  development: "mobile:bots.app.devMark",
  noEgress: "mobile:bots.app.noEgressMark",
  autoUpdate: "mobile:bots.app.autoUpdateMark",
};

/**
 * A standing fact about a bot's approved App (#2005), drawn like the Bot badge beside it: a quiet
 * tag, never the accent, the words carrying the meaning. "Development build" follows the bot
 * everywhere its badge does for every member; "Can't send data outside this group" sits on its
 * profile; "Updates approve themselves" on the admin's App card.
 */
export function AppMark({ kind, testID }: { kind: AppMarkKind; testID?: string }) {
  const { t } = useTranslation("mobile");
  const glyph =
    kind === "development" ? (
      <Icon.key size={12} color={semantic.dim} />
    ) : kind === "noEgress" ? (
      <Icon.lock size={12} color={semantic.dim} />
    ) : (
      <Icon.history size={12} color={semantic.dim} />
    );
  return <Tag testID={testID} tone="neutral" label={t(LABEL_KEYS[kind])} leading={glyph} />;
}
