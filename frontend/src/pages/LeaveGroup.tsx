import { errorMessage } from "../utils/errorMessage";
import React from "react";
import { Trans, useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useLeaveGroup, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Button } from "../components/ui/Button";
import { PageShell } from "../components/Layout/PageShell";

export const LeaveGroupPage: React.FC = observer(() => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/leave" });
  const { setSelectedGroupId, setSelectedChannelId } = appStore;
  const leaveGroupMutation = useLeaveGroup();

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  if (isLoading || !group) {
    return null;
  }

  return (
    <PageShell title={t("leaveGroup.pageTitle")}>
      <div className="h-full flex flex-col items-center justify-center gap-4 px-6">
        <p className="text-xs font-mono text-center text-dim">
          <Trans
            i18nKey="channels:leaveGroup.confirm"
            values={{ name: group.name }}
            components={{ strong: <strong /> }}
          />
          <br />
          {t("leaveGroup.rejoinNotice")}
        </p>
        {leaveGroupMutation.isError && (
          <p className="text-xs font-mono text-danger">
            {errorMessage(leaveGroupMutation.error, t("leaveGroup.leaveFailed"))}
          </p>
        )}
        <div className="flex gap-3">
          <Button
            data-testid="leave-group-confirm"
            variant="danger"
            onClick={async () => {
              try {
                await leaveGroupMutation.mutateAsync(group.id);
                setSelectedGroupId(null);
                setSelectedChannelId(null);
                navigate({ to: "/" });
              } catch {
                // error shown via isError above
              }
            }}
            disabled={leaveGroupMutation.isPending}
            isLoading={leaveGroupMutation.isPending}
            loadingText={t("leaveGroup.submitting")}
          >
            {t("leaveGroup.confirmButton")}
          </Button>
          <Button
            data-testid="leave-group-cancel"
            variant="secondary"
            onClick={() => navigate({ to: "/groups/$groupId", params: { groupId } })}
          >
            {t("common:actions.cancel")}
          </Button>
        </div>
      </div>
    </PageShell>
  );
});
