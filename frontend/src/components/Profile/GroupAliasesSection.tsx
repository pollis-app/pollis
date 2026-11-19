import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { observer } from "mobx-react-lite";
import { Avatar } from "../ui/Avatar";
import { Button } from "../ui/Button";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useMyAliases, useResetGroupAlias } from "../../hooks/queries/useGroupAliases";
import { appStore } from "../../stores/appStore";

interface GroupAliasesSectionProps {
  /** The main profile, which a group without an alias shows. */
  mainName: string;
  mainAvatarKey: string | null;
}

/**
 * The profile's "Group aliases" section (#2003): one row per group the user is in,
 * with what that group shows (alias or main profile), Edit, and "Use my main profile".
 */
export const GroupAliasesSection: React.FC<GroupAliasesSectionProps> = observer(
  ({ mainName, mainAvatarKey }) => {
    const { t } = useTranslation("settings");
    const navigate = useNavigate();
    const { data: groups = [] } = useUserGroupsWithChannels();
    const { data: aliases } = useMyAliases();
    const reset = useResetGroupAlias();
    const userId = appStore.currentUser?.id;

    return (
      <section className="flex flex-col gap-4 mb-12" data-testid="settings-aliases">
        <h2 className="text-secondary font-mono font-medium uppercase tracking-widest text-dim pb-1 border-b border-line">
          {t("aliases.heading")}
        </h2>
        <p className="text-secondary text-muted">{t("aliases.intro")}</p>
        {groups.length === 0 ? (
          <p data-testid="settings-aliases-empty" className="text-secondary text-muted">
            {t("aliases.empty")}
          </p>
        ) : (
          <ul className="flex flex-col gap-3">
            {groups.map((g) => {
              const alias = aliases?.get(g.id);
              const name = alias?.display_name || mainName;
              const avatarKey = alias?.avatar_url || mainAvatarKey;
              const resetting = reset.isPending && reset.variables?.groupId === g.id;
              return (
                <li
                  key={g.id}
                  data-testid={`alias-row-${g.id}`}
                  className="flex items-center gap-3 min-w-0"
                >
                  <Avatar avatarKey={avatarKey} size={32} alt="" />
                  <span className="flex flex-col min-w-0 flex-1">
                    <span className="truncate text-fg">{g.name}</span>
                    <span
                      data-testid={`alias-summary-${g.id}`}
                      className="truncate text-secondary text-muted"
                    >
                      {alias ? <bdi>{name}</bdi> : t("aliases.mainProfile")}
                    </span>
                  </span>
                  {alias && userId && (
                    <Button
                      size="sm"
                      variant="ghost"
                      data-testid={`alias-reset-${g.id}`}
                      disabled={reset.isPending && !resetting}
                      isLoading={resetting}
                      loadingText={t("aliases.resetting")}
                      onClick={() => reset.mutate({ groupId: g.id, userId })}
                    >
                      {t("aliases.useMain")}
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant="secondary"
                    data-testid={`alias-edit-${g.id}`}
                    onClick={() =>
                      navigate({ to: "/user/aliases/$groupId", params: { groupId: g.id } })
                    }
                  >
                    {t("aliases.edit")}
                  </Button>
                </li>
              );
            })}
          </ul>
        )}
        {reset.isError && (
          <p data-testid="alias-reset-error" className="text-secondary font-mono text-danger">
            {t("aliases.saveFailed")}
          </p>
        )}
      </section>
    );
  },
);
