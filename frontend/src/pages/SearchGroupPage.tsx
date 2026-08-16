import { errorMessage } from "../utils/errorMessage";
import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { invoke } from "../bridge";
import { useQueryClient } from "@tanstack/react-query";
import { PageShell } from "../components/Layout/PageShell";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useRequestGroupAccess, useMyJoinRequest, useUserGroupsWithChannels, groupQueryKeys } from "../hooks/queries";
import { deriveSlug } from "../utils/urlRouting";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";
import { Card } from "../components/ui/Card";

export const SearchGroupPage: React.FC = observer(() => {
  const { t } = useTranslation("search");
  const navigate = useNavigate();
  const currentUser = appStore.currentUser;
  const queryClient = useQueryClient();
  const { data: userGroups } = useUserGroupsWithChannels();
  const [slug, setSlug] = useState("");
  const [isSearching, setIsSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [foundGroup, setFoundGroup] = useState<{ id: string; name: string; description?: string } | null>(null);

  const requestAccessMutation = useRequestGroupAccess();
  const { data: myJoinRequest } = useMyJoinRequest(foundGroup?.id);

  const isMember = foundGroup != null && (userGroups ?? []).some((g) => g.id === foundGroup.id);

  const handleSearch = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (!slug.trim()) {
      setSearchError(t("group.slugRequired"));
      return;
    }
    setIsSearching(true);
    setSearchError(null);
    setFoundGroup(null);
    try {
      const group = await invoke<{ id: string; name: string; description?: string }>('search_group_by_slug', { slug: slug.trim() });
      setFoundGroup(group);
    } catch (err) {
      setSearchError(errorMessage(err, t("group.notFound")));
    } finally {
      setIsSearching(false);
    }
  };

  const handleRequestAccess = async () => {
    if (!foundGroup || !currentUser) {
      return;
    }
    await requestAccessMutation.mutateAsync(foundGroup.id);
    queryClient.invalidateQueries({
      queryKey: groupQueryKeys.myJoinRequest(foundGroup.id, currentUser.id),
    });
  };

  return (
    <PageShell title={t("group.title")}>
      <div
        data-testid="search-group-page"
        className="flex-1 flex flex-col overflow-auto bg-bg"
      >
        <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
          <div className="w-full max-w-md flex flex-col gap-6">

            <form onSubmit={handleSearch} className="flex flex-col gap-3">
              <TextInput
                label={t("group.slugLabel")}
                value={slug}
                onChange={setSlug}
                placeholder={t("group.slugPlaceholder")}
                disabled={isSearching}
                id="search-group-slug"
              />
              <input data-testid="search-group-slug-input" type="hidden" value={slug} readOnly />

              <Button
                data-testid="search-group-button"
                type="submit"
                disabled={!slug.trim() || isSearching}
                isLoading={isSearching}
                loadingText={t("group.searching")}
              >
                {t("group.submit")}
              </Button>
            </form>

            {foundGroup && (
              <Card
                data-testid="search-group-result"
                className="flex flex-col gap-3"
                padding="sm"
              >
                <div className="flex flex-col gap-0.5">
                  <h2 className="text-sm font-mono font-medium text-accent">
                    {foundGroup.name}
                  </h2>
                  <p className="text-xs font-mono text-muted">
                    /g/{deriveSlug(foundGroup.name)}
                  </p>
                  {foundGroup.description && (
                    <p className="text-xs font-mono mt-1 text-dim">
                      {foundGroup.description}
                    </p>
                  )}
                </div>
                {isMember ? (
                  <Button
                    data-testid="go-to-group-button"
                    onClick={() => navigate({ to: "/groups/$groupId", params: { groupId: foundGroup.id } })}
                  >
                    {t("group.goToGroup")}
                  </Button>
                ) : myJoinRequest?.status === "pending" ? (
                  <p
                    data-testid="request-pending-indicator"
                    className="text-xs font-mono text-muted"
                  >
                    {t("group.requestPending")}
                  </p>
                ) : myJoinRequest?.status === "rejected" ? (
                  <div className="flex flex-col gap-2">
                    <p
                      data-testid="request-rejected-indicator"
                      className="text-xs font-mono text-danger"
                    >
                      {t("group.requestRejected")}
                    </p>
                    <Button
                      data-testid="try-again-button"
                      onClick={handleRequestAccess}
                      disabled={requestAccessMutation.isPending}
                      isLoading={requestAccessMutation.isPending}
                      loadingText={t("group.sendingRequest")}
                    >
                      {t("group.tryAgain")}
                    </Button>
                  </div>
                ) : (
                  <Button
                    data-testid="request-access-button"
                    onClick={handleRequestAccess}
                    disabled={requestAccessMutation.isPending}
                    isLoading={requestAccessMutation.isPending}
                    loadingText={t("group.sendingRequest")}
                  >
                    {t("group.requestAccess")}
                  </Button>
                )}
              </Card>
            )}

            {searchError && (
              <p data-testid="search-group-error" className="text-xs font-mono text-danger">
                {searchError}
              </p>
            )}
          </div>
        </div>
      </div>
    </PageShell>
  );
});
