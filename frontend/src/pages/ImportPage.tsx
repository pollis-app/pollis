import React, { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { dialogOpen, invoke } from "../bridge";
import { PageShell } from "../components/Layout/PageShell";
import { PageColumn } from "../components/ui/PageColumn";
import { FormLead } from "../components/ui/FormLead";
import { Button } from "../components/ui/Button";
import { RadioList } from "../components/ui/RadioList";
import { Switch } from "../components/ui/Switch";
import { LoadingSpinner } from "../components/ui/LoaderSpinner";
import { ImportPlanView } from "../components/Import/ImportPlanView";
import { ImportResultView } from "../components/Import/ImportResultView";
import { appStore } from "../stores/appStore";
import { groupQueryKeys } from "../hooks/queries/useGroups";
import { errorMessage } from "../utils/errorMessage";
import {
  IMPORT_TARGET_NEW,
  type ImportApplyOptions,
  type ImportPreview,
  type ImportResult,
  type ImportSource,
} from "../types/import";

interface ImportPageProps {
  source: ImportSource;
}

/**
 * Import a community's structure from Discord (#1180) or Slack (#1179): choose the export, read the
 * dry run, choose where it goes, apply. A page, never a dialog; nothing is written before Import.
 */
export const ImportPage: React.FC<ImportPageProps> = observer(({ source }) => {
  const { t } = useTranslation("import");
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const userId = appStore.currentUser?.id ?? null;
  const slack = source === "slack";

  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [reading, setReading] = useState(false);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<ImportResult | null>(null);
  const [inviteByEmail, setInviteByEmail] = useState(true);
  // A newer read wins over a slower older one.
  const readSeq = useRef(0);

  const read = async (path: string, target: string | null, includeArchived: boolean) => {
    if (!userId) {
      return;
    }
    const seq = ++readSeq.current;
    setReading(true);
    setError(null);
    try {
      const p = await invoke<ImportPreview>("import_preview", { source, path, userId, target, includeArchived });
      if (seq === readSeq.current) {
        setPreview(p);
      }
    } catch (err) {
      if (seq === readSeq.current) {
        setError(t("page.readFailed", { message: errorMessage(err) }));
      }
    } finally {
      if (seq === readSeq.current) {
        setReading(false);
      }
    }
  };

  const choose = async (kind: "folder" | "zip") => {
    const picked = await dialogOpen(
      kind === "zip"
        ? { title: t("slack.chooseZip"), filters: [{ name: t("slack.zipFilter"), extensions: ["zip"] }] }
        : { directory: true, title: t(`${source}.chooseFolder`) },
    );
    if (typeof picked !== "string") {
      return;
    }
    setResult(null);
    setPreview(null);
    await read(picked, null, false);
  };

  const target = preview ? (preview.target?.group_id ?? IMPORT_TARGET_NEW) : IMPORT_TARGET_NEW;
  const plan = preview?.plan;
  const sendsInvites = slack && inviteByEmail && (plan?.totals.people_with_email ?? 0) > 0;

  const apply = async () => {
    if (!preview || !userId) {
      return;
    }
    setApplying(true);
    setError(null);
    try {
      const options: ImportApplyOptions = { include_archived: preview.include_archived, invite_by_email: sendsInvites };
      const r = await invoke<ImportResult>("import_apply", { source, path: preview.path, userId, target, options });
      setResult(r);
      // The sidebar tree (groups, channels, categories), the emoji the picker offers, and the invites sent.
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroupsWithChannels(userId) });
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroups(userId) });
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.channels(r.group_id) });
      queryClient.invalidateQueries({ queryKey: ["emoji"] });
    } catch (err) {
      setError(t("page.applyFailed", { message: errorMessage(err) }));
    } finally {
      setApplying(false);
    }
  };

  // Email invites may still reach someone who joined Pollis since, so a run that sends them is never "nothing".
  const nothingToDo =
    !!plan &&
    !!preview?.target &&
    !sendsInvites &&
    plan.totals.to_create_categories + plan.totals.to_create_channels + plan.totals.to_upload_emoji === 0;
  const archivedCount = plan
    ? plan.totals.archived_skipped + plan.channels.filter((c) => c.archived).length
    : 0;
  const withCategories = source === "discord";

  return (
    <PageShell title={t(`${source}.pageTitle`)} scrollable>
      <PageColumn spacing="sections" paneTestId={`${source}-import-page`}>
        <FormLead title={t(`${source}.leadTitle`)}>{t(`${source}.lead`)}</FormLead>
        <p className="text-secondary leading-relaxed text-muted -mt-4">{t(`${source}.howTo`)}</p>

        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap gap-2">
            {slack && (
              <Button
                variant={preview ? "ghost" : "primary"}
                onClick={() => choose("zip")}
                disabled={reading || applying}
                data-testid="import-choose-zip"
              >
                {t("slack.chooseZip")}
              </Button>
            )}
            <Button
              variant={preview || slack ? "ghost" : "primary"}
              onClick={() => choose("folder")}
              disabled={reading || applying}
              data-testid="import-choose-folder"
            >
              {preview && !slack ? t("page.chooseAnother") : t(`${source}.chooseFolder`)}
            </Button>
          </div>
          {preview && (
            <p className="text-secondary text-muted break-all" data-testid="import-folder">
              {t("page.export")}: <span className="font-machine">{preview.path}</span>
            </p>
          )}
          {reading && (
            <p className="flex items-center gap-2 text-secondary text-muted" data-testid="import-reading">
              <LoadingSpinner size="sm" />
              {t("page.reading")}
            </p>
          )}
          {error && (
            <p className="text-secondary text-danger" data-testid="import-error">
              {error}
            </p>
          )}
        </div>

        {result && (
          <ImportResultView
            result={result}
            withCategories={withCategories}
            onOpenGroup={() => {
              appStore.setSelectedGroupId(result.group_id);
              navigate({ to: "/groups/$groupId", params: { groupId: result.group_id } });
            }}
          />
        )}

        {preview && plan && !result && (
          <>
            <p className="text-body text-fg" data-testid="import-summary">
              {t(withCategories ? "page.summary" : "page.summaryNoCategories", {
                name: plan.group.name,
                categories: plan.totals.categories,
                channels: plan.totals.channels,
                emoji: plan.totals.emoji,
                people: plan.totals.people,
              })}
            </p>

            {preview.candidates.length > 0 && (
              <div className="flex flex-col gap-2">
                <h3 className="text-heading font-semibold text-fg">{t("page.targetTitle")}</h3>
                <RadioList
                  aria-label={t("page.targetTitle")}
                  data-testid="import-target"
                  value={target}
                  onChange={(v) => {
                    if (v !== target) {
                      void read(preview.path, v, preview.include_archived);
                    }
                  }}
                  options={[
                    ...preview.candidates.map((c) => ({
                      value: c.group_id,
                      label: t("page.targetExisting", { name: c.name }),
                      detail: t("page.targetExistingDetail"),
                      disabled: reading,
                    })),
                    {
                      value: IMPORT_TARGET_NEW,
                      label: t("page.targetNew", { name: plan.group.name }),
                      detail: t("page.targetNewDetail"),
                      disabled: reading,
                    },
                  ]}
                />
              </div>
            )}

            {slack && archivedCount > 0 && (
              <div className="flex flex-col gap-2">
                <h3 className="text-heading font-semibold text-fg">{t("slack.archivedTitle")}</h3>
                <Switch
                  label={t("slack.includeArchived")}
                  description={
                    preview.include_archived
                      ? t("slack.includeArchivedOn")
                      : t("slack.includeArchivedDetail", { count: archivedCount })
                  }
                  checked={preview.include_archived}
                  disabled={reading || applying}
                  onChange={(on) => void read(preview.path, target, on)}
                  data-testid="import-include-archived"
                />
              </div>
            )}

            {slack && plan.totals.people_with_email > 0 && (
              <div className="flex flex-col gap-2">
                <h3 className="text-heading font-semibold text-fg">{t("slack.inviteTitle")}</h3>
                <Switch
                  label={t("slack.inviteByEmail")}
                  description={t("slack.inviteByEmailDetail", { count: plan.totals.people_with_email })}
                  checked={inviteByEmail}
                  disabled={applying}
                  onChange={setInviteByEmail}
                  data-testid="import-invite-by-email"
                />
              </div>
            )}

            {nothingToDo && (
              <p className="text-secondary text-muted" data-testid="import-nothing-to-do">
                {t("page.nothingToDo")}
              </p>
            )}

            <div>
              <Button
                onClick={apply}
                isLoading={applying}
                loadingText={t("page.applying")}
                disabled={reading}
                data-testid="import-apply"
              >
                {preview.target ? t("page.applyMore") : t("page.apply")}
              </Button>
            </div>

            <ImportPlanView plan={plan} inviteSheet={preview.invite_sheet} />
          </>
        )}
      </PageColumn>
    </PageShell>
  );
});
