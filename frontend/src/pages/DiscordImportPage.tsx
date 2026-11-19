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
import { LoadingSpinner } from "../components/ui/LoaderSpinner";
import { ImportPlanView } from "../components/Import/ImportPlanView";
import { ImportResultView } from "../components/Import/ImportResultView";
import { appStore } from "../stores/appStore";
import { groupQueryKeys } from "../hooks/queries/useGroups";
import { errorMessage } from "../utils/errorMessage";
import { IMPORT_TARGET_NEW, type DiscordImportPreview, type DiscordImportResult } from "../types/import";

/**
 * Import a Discord server's structure (#1180): pick a DiscordChatExporter folder, read the dry
 * run, choose where it goes, apply. A page, never a dialog; nothing is written before Import.
 */
export const DiscordImportPage: React.FC = observer(() => {
  const { t } = useTranslation("import");
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const userId = appStore.currentUser?.id ?? null;

  const [preview, setPreview] = useState<DiscordImportPreview | null>(null);
  const [reading, setReading] = useState(false);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<DiscordImportResult | null>(null);
  // A newer read wins over a slower older one.
  const readSeq = useRef(0);

  const read = async (dir: string, target: string | null) => {
    if (!userId) {
      return;
    }
    const seq = ++readSeq.current;
    setReading(true);
    setError(null);
    try {
      const p = await invoke<DiscordImportPreview>("discord_import_preview", { dir, userId, target });
      if (seq === readSeq.current) {
        setPreview(p);
      }
    } catch (err) {
      if (seq === readSeq.current) {
        setError(t("discord.readFailed", { message: errorMessage(err) }));
      }
    } finally {
      if (seq === readSeq.current) {
        setReading(false);
      }
    }
  };

  const choose = async () => {
    const picked = await dialogOpen({ directory: true, title: t("discord.chooseFolder") });
    if (typeof picked !== "string") {
      return;
    }
    setResult(null);
    setPreview(null);
    await read(picked, null);
  };

  const target = preview ? (preview.target?.group_id ?? IMPORT_TARGET_NEW) : IMPORT_TARGET_NEW;

  const apply = async () => {
    if (!preview || !userId) {
      return;
    }
    setApplying(true);
    setError(null);
    try {
      const r = await invoke<DiscordImportResult>("discord_import_apply", { dir: preview.dir, userId, target });
      setResult(r);
      // The sidebar tree (groups, channels, categories) and the emoji the picker offers.
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroupsWithChannels(userId) });
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroups(userId) });
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.channels(r.group_id) });
      queryClient.invalidateQueries({ queryKey: ["emoji"] });
    } catch (err) {
      setError(t("discord.applyFailed", { message: errorMessage(err) }));
    } finally {
      setApplying(false);
    }
  };

  const plan = preview?.plan;
  const nothingToDo =
    !!plan &&
    !!preview?.target &&
    plan.totals.to_create_categories + plan.totals.to_create_channels + plan.totals.to_upload_emoji === 0;

  return (
    <PageShell title={t("discord.pageTitle")} scrollable>
      <PageColumn spacing="sections" paneTestId="discord-import-page">
        <FormLead title={t("discord.leadTitle")}>{t("discord.lead")}</FormLead>
        <p className="text-secondary leading-relaxed text-muted -mt-4">{t("discord.howTo")}</p>

        <div className="flex flex-col gap-2">
          <div>
            <Button
              variant={preview ? "ghost" : "primary"}
              onClick={choose}
              disabled={reading || applying}
              data-testid="import-choose-folder"
            >
              {preview ? t("discord.chooseAnother") : t("discord.chooseFolder")}
            </Button>
          </div>
          {preview && (
            <p className="text-secondary text-muted break-all" data-testid="import-folder">
              {t("discord.folder")}: <span className="font-machine">{preview.dir}</span>
            </p>
          )}
          {reading && (
            <p className="flex items-center gap-2 text-secondary text-muted" data-testid="import-reading">
              <LoadingSpinner size="sm" />
              {t("discord.reading")}
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
            onOpenGroup={() => {
              appStore.setSelectedGroupId(result.group_id);
              navigate({ to: "/groups/$groupId", params: { groupId: result.group_id } });
            }}
          />
        )}

        {preview && plan && !result && (
          <>
            <p className="text-body text-fg" data-testid="import-summary">
              {t("discord.summary", {
                name: plan.group.name,
                categories: plan.totals.categories,
                channels: plan.totals.channels,
                emoji: plan.totals.emoji,
                people: plan.totals.people,
              })}
            </p>

            {preview.candidates.length > 0 && (
              <div className="flex flex-col gap-2">
                <h3 className="text-heading font-semibold text-fg">{t("discord.targetTitle")}</h3>
                <RadioList
                  aria-label={t("discord.targetTitle")}
                  data-testid="import-target"
                  value={target}
                  onChange={(v) => {
                    if (v !== target) {
                      void read(preview.dir, v);
                    }
                  }}
                  options={[
                    ...preview.candidates.map((c) => ({
                      value: c.group_id,
                      label: t("discord.targetExisting", { name: c.name }),
                      detail: t("discord.targetExistingDetail"),
                      disabled: reading,
                    })),
                    {
                      value: IMPORT_TARGET_NEW,
                      label: t("discord.targetNew", { name: plan.group.name }),
                      detail: t("discord.targetNewDetail"),
                      disabled: reading,
                    },
                  ]}
                />
              </div>
            )}

            {nothingToDo && (
              <p className="text-secondary text-muted" data-testid="import-nothing-to-do">
                {t("discord.nothingToDo")}
              </p>
            )}

            <div>
              <Button
                onClick={apply}
                isLoading={applying}
                loadingText={t("discord.applying")}
                disabled={reading}
                data-testid="import-apply"
              >
                {preview.target ? t("discord.applyMore") : t("discord.apply")}
              </Button>
            </div>

            <ImportPlanView plan={plan} inviteSheet={preview.invite_sheet} />
          </>
        )}
      </PageColumn>
    </PageShell>
  );
});
