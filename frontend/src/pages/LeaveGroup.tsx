import { errorMessage } from "../utils/errorMessage";
import React from "react";
import { Trans, useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useLeaveGroup, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { ConfirmPage } from "../components/Layout/ConfirmPage";

export const LeaveGroupPage: React.FC = observer(() => {
  const { t } = useTranslation("channels");
  const { finish } = useFinishFlow();
  const { groupId } = useParams({ from: "/groups/$groupId/leave" });
  const { setSelectedGroupId, setSelectedChannelId } = appStore;
  const leaveGroupMutation = useLeaveGroup();

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  if (isLoading || !group) {
    return null;
  }

  return (
    <ConfirmPage
      title={t("leaveGroup.pageTitle")}
      testId="leave-group-confirmation"
      question={
        <Trans
          i18nKey="channels:leaveGroup.confirm"
          values={{ name: group.name }}
          components={{ strong: <strong /> }}
        />
      }
      consequence={t("leaveGroup.rejoinNotice")}
      error={
        leaveGroupMutation.isError
          ? errorMessage(leaveGroupMutation.error, t("leaveGroup.leaveFailed"))
          : null
      }
      confirmTestId="leave-group-confirm"
      confirmLabel={t("leaveGroup.confirmButton")}
      pendingLabel={t("leaveGroup.submitting")}
      isPending={leaveGroupMutation.isPending}
      onConfirm={async () => {
        try {
          await leaveGroupMutation.mutateAsync(group.id);
          setSelectedGroupId(null);
          setSelectedChannelId(null);
          finish({ to: "/" });
        } catch {
          // error shown via isError above
        }
      }}
      cancel={{
        testId: "leave-group-cancel",
        onClick: () => finish({ to: "/groups/$groupId", params: { groupId } }),
      }}
    />
  );
});
