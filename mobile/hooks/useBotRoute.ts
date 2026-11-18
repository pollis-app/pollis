import { useTranslation } from "react-i18next";
import { useNav, useRouteParams } from "../components/pane/paneContext";
import { useGroupAccess } from "./useGroupAccess";

/** The Bots pages' route (#1182): `groupId`, an optional `botId`, the group, its name, and admin. */
export function useBotRoute() {
  const { t } = useTranslation("mobile");
  const router = useNav();
  const { groupId, botId } = useRouteParams<{ groupId?: string; botId?: string }>();
  const gid = groupId ?? null;
  const bid = botId ?? null;
  const { group, members, iAmAdmin } = useGroupAccess(gid);
  const groupName = group?.name ?? t("group.common.fallbackName");
  return { t, router, gid, bid, group, groupName, members, iAmAdmin };
}
