import { useState } from "react";
import { useNav } from "../pane/paneContext";
import { useTranslation } from "react-i18next";
import { SheetOverlay, afterSheetClose } from "../chat/SheetOverlay";
import { Group, ListRow } from "../ui";
import { Icon } from "../icons";
import { ErrorText } from "./FormBits";
import { semantic } from "../../theme/tokens";
import { useLeaveGroup } from "../../hooks/queries";
import { haptic } from "../../lib/haptics";

// The group header's menu; Leave group sits apart and needs a second tap.
export function GroupMenuSheet({
  groupId,
  groupName,
  memberCount,
  pendingRequests,
  onClose,
}: {
  groupId: string;
  groupName: string;
  // Undefined while the roster is loading: the row shows no count.
  memberCount?: number;
  pendingRequests: number;
  onClose: () => void;
}) {
  const { t } = useTranslation("mobile");
  const router = useNav();
  const leaveGroup = useLeaveGroup();
  const [armed, setArmed] = useState(false);

  const go = (pathname: "/group/members" | "/group/invite" | "/group/settings" | "/group/requests") => {
    onClose();
    afterSheetClose(() => router.push({ pathname, params: { groupId } }));
  };

  const onLeave = () => {
    if (!armed) {
      haptic("confirmArm");
      setArmed(true);
      return;
    }
    leaveGroup.mutate(groupId, {
      onSuccess: () => {
        onClose();
        afterSheetClose(() => router.exitToTab("groups"));
      },
    });
  };

  const leaveLabel = leaveGroup.isPending
    ? t("group.panel.leaving")
    : armed
      ? t("group.panel.leaveConfirm", { name: groupName })
      : t("group.panel.leave");

  return (
    <SheetOverlay title={groupName} onClose={onClose} testID="sheet-group-menu">
      {/* Navigation rows in a sheet that does not scroll: touch-down (#1529).
          Leave, below, stays on release. */}
      <Group surface="high">
        <ListRow
          testID="btn-menu-members"
          pressIn
          glyph={<Icon.users size={22} color={semantic.dim} />}
          name={t("group.panel.members")}
          value={memberCount !== undefined ? String(memberCount) : undefined}
          onPress={() => go("/group/members")}
        />
        <ListRow
          testID="btn-menu-invite"
          pressIn
          glyph={<Icon.userPlus size={22} color={semantic.dim} />}
          name={t("group.panel.invite")}
          onPress={() => go("/group/invite")}
        />
        <ListRow
          testID="btn-menu-group-settings"
          pressIn
          glyph={<Icon.sliders size={22} color={semantic.dim} />}
          name={t("group.panel.settings")}
          onPress={() => go("/group/settings")}
        />
        {pendingRequests > 0 ? (
          <ListRow
            testID="btn-menu-requests"
            pressIn
            glyph={<Icon.inbox size={22} color={semantic.dim} />}
            name={t("group.panel.joinRequests")}
            badge={pendingRequests}
            accessibilityLabel={`${t("group.panel.joinRequests")}, ${t("channels:groups.joinRequestsPending", { count: pendingRequests })}`}
            onPress={() => go("/group/requests")}
          />
        ) : null}
      </Group>
      <Group surface="high">
        <ListRow
          testID="btn-leave-group"
          glyph={<Icon.logOut size={22} color={semantic.accent} />}
          name={leaveLabel}
          nameStyle={{ color: semantic.accent }}
          sub={t("group.panel.leaveHint")}
          disabled={leaveGroup.isPending}
          onPress={onLeave}
        />
      </Group>
      {leaveGroup.isError ? (
        <ErrorText>
          {(leaveGroup.error as Error).message || t("channels:leaveGroup.leaveFailed")}
        </ErrorText>
      ) : null}
    </SheetOverlay>
  );
}
