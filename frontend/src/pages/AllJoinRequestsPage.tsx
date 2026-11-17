import React, { useMemo } from "react";
import { Trans, useTranslation } from "react-i18next";
import { PageShell } from "../components/Layout/PageShell";
import { useAllPendingJoinRequests, useApproveJoinRequest, useRejectJoinRequest } from "../hooks/queries";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";
import { errorMessage } from "../utils/errorMessage";
import { EmptyState } from "../components/ui/EmptyState";

export const AllJoinRequestsPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { data: allRequests = [], isLoading } = useAllPendingJoinRequests();
  const { data: groupsWithChannels = [] } = useUserGroupsWithChannels();
  const approveMutation = useApproveJoinRequest();
  const rejectMutation = useRejectJoinRequest();
  const decisionError = approveMutation.error ?? rejectMutation.error;

  // Build a map of groupId → group name for display
  const groupNameById = useMemo(() => {
    const map: Record<string, string> = {};
    for (const g of groupsWithChannels) {
      map[g.id] = g.name;
    }
    return map;
  }, [groupsWithChannels]);

  // Group requests by group_id
  const requestsByGroup = useMemo(() => {
    const grouped: Record<string, typeof allRequests> = {};
    for (const req of allRequests) {
      if (!grouped[req.group_id]) {
        grouped[req.group_id] = [];
      }
      grouped[req.group_id].push(req);
    }
    return grouped;
  }, [allRequests]);

  const groupIds = Object.keys(requestsByGroup);

  const handleApprove = async (requestId: string, groupId: string) => {
    try {
      await approveMutation.mutateAsync({ requestId, groupId });
    } catch (err) {
      console.error("Failed to approve request:", err);
    }
  };

  const handleReject = async (requestId: string, groupId: string) => {
    try {
      await rejectMutation.mutateAsync({ requestId, groupId });
    } catch (err) {
      console.error("Failed to reject request:", err);
    }
  };

  if (isLoading) {
    return (
      <PageShell title={t("joinRequests.allPageTitle")}>
        <div
          data-testid="all-join-requests-page"
          className="flex-1 flex flex-col overflow-auto bg-bg"
        >
          <div className="px-4 py-4">
            <p className="text-secondary font-mono text-muted">
              {t("common:states.loading")}
            </p>
          </div>
        </div>
      </PageShell>
    );
  }

  if (groupIds.length === 0) {
    return (
      <PageShell title={t("joinRequests.allPageTitle")}>
        <div
          data-testid="all-join-requests-page"
          className="flex-1 flex flex-col overflow-auto bg-bg"
        >
          <EmptyState background={false} testId="all-join-requests-empty">
            {t("joinRequests.allEmpty")}
          </EmptyState>
        </div>
      </PageShell>
    );
  }

  return (
    <PageShell title={t("joinRequests.allPageTitle")}>
      <div
        data-testid="all-join-requests-page"
        className="flex-1 flex flex-col overflow-auto bg-bg"
      >
        {groupIds.map((groupId) => {
          const requests = requestsByGroup[groupId];
          const groupName = groupNameById[groupId] ?? groupId;

          return (
            <div key={groupId} className="flex flex-col">
              <div className="px-4 py-4">
                <p className="text-secondary font-mono text-dim">
                  <Trans
                    i18nKey="channels:joinRequests.pendingFor"
                    values={{ name: groupName }}
                    components={{ accent: <span className="text-accent" /> }}
                  />
                </p>
              </div>

              <NavigableList
                items={requests}
                getKey={(req) => req.id}
                rowTestId={(req) => `join-request-${req.id}`}
                renderRow={(req) => (
                  <span
                    className="flex-1 truncate text-secondary font-mono text-fg"
                  >
                    {req.requester_username ?? req.requester_id}
                  </span>
                )}
                controls={(req) => [
                  <Button size="sm"
                    data-testid={`approve-request-${req.id}`}
                    onClick={() => handleApprove(req.id, req.group_id)}
                    disabled={approveMutation.isPending || rejectMutation.isPending}
                    isLoading={approveMutation.isPending && approveMutation.variables?.requestId === req.id}
                    variant="primary"
                  >
                    {t("joinRequests.approve")}
                  </Button>,
                  <Button size="sm"
                    data-testid={`reject-request-${req.id}`}
                    onClick={() => handleReject(req.id, req.group_id)}
                    disabled={approveMutation.isPending || rejectMutation.isPending}
                    isLoading={rejectMutation.isPending && rejectMutation.variables?.requestId === req.id}
                    variant="danger"
                  >
                    {t("joinRequests.reject")}
                  </Button>,
                ]}
              />
            </div>
          );
        })}
        {decisionError && (
          <p data-testid="join-request-error" className="px-4 py-2 text-secondary font-mono text-danger" role="alert">
            {errorMessage(decisionError)}
          </p>
        )}
      </div>
    </PageShell>
  );
};
