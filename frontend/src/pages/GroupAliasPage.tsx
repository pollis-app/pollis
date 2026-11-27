import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { observer } from "mobx-react-lite";
import { Upload, User } from "lucide-react";
import { PageShell } from "../components/Layout/PageShell";
import { PageColumn } from "../components/ui/PageColumn";
import { TextInput } from "../components/ui/TextInput";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { useMyAliases, useResetGroupAlias, useSetGroupAlias } from "../hooks/queries/useGroupAliases";
import { useAvatarBlobUrl, useUserProfile } from "../hooks/queries/useUserProfile";
import { uploadAvatar } from "../services/r2-upload";
import { resizeImage } from "../utils/imageProcessing";
import { fieldWrite, isValidAliasName } from "../utils/groupAlias";
import { errorMessage } from "../utils/errorMessage";
import { appStore } from "../stores/appStore";

/**
 * Edit the signed-in user's alias in one group (#2003): a name and a picture shown
 * only there. "Apply to all my groups" writes the changed field(s) to every group the
 * user is in now; "Use my main profile" removes the alias here.
 */
export const GroupAliasPage: React.FC = observer(() => {
  const { t } = useTranslation("settings");
  const { finish } = useFinishFlow();
  const { groupId } = useParams({ from: "/user/aliases/$groupId" });
  const currentUser = appStore.currentUser;
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: aliases, isLoading } = useMyAliases();
  const { data: profile } = useUserProfile();
  const setAlias = useSetGroupAlias();
  const reset = useResetGroupAlias();

  const group = groups.find((g) => g.id === groupId);
  const alias = aliases?.get(groupId);
  const mainName = profile?.preferred_name || profile?.username || "";

  const [name, setName] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const [file, setFile] = useState<File | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [clearPicture, setClearPicture] = useState(false);
  const [applyAll, setApplyAll] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [uploading, setUploading] = useState(false);
  const seeded = useRef(false);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Seed the field once the stored alias is known.
  useEffect(() => {
    if (!seeded.current && !isLoading) {
      seeded.current = true;
      setName(alias?.display_name ?? "");
    }
  }, [alias, isLoading]);

  useEffect(() => {
    return () => {
      if (preview) {
        URL.revokeObjectURL(preview);
      }
    };
  }, [preview]);

  const shownKey = clearPicture ? profile?.avatar_url ?? null : alias?.avatar_url || profile?.avatar_url || null;
  const { data: shownUrl } = useAvatarBlobUrl(shownKey);
  const busy = setAlias.isPending || uploading;

  const back = () => finish({ to: "/user" });

  const handleSave = async () => {
    if (!currentUser) {
      return;
    }
    const trimmed = name.trim();
    if (trimmed && !isValidAliasName(trimmed)) {
      setNameError(t("aliases.nameInvalid"));
      return;
    }
    setNameError(null);
    setError(null);
    setSaved(false);
    // Here: only what changed. Everywhere: the field the user filled in or changed.
    const nameChanged = fieldWrite(trimmed || null, alias?.display_name);
    const displayName = applyAll ? (trimmed || nameChanged !== undefined ? trimmed : undefined) : nameChanged;
    let avatarUrl: string | undefined;
    try {
      if (file) {
        setUploading(true);
        const optimized = await resizeImage(file);
        avatarUrl = (await uploadAvatar(currentUser.id, groupId, optimized)).object_key;
      } else if (clearPicture && (applyAll || alias?.avatar_url)) {
        avatarUrl = "";
      }
      if (displayName === undefined && avatarUrl === undefined) {
        setSaved(true);
        return;
      }
      await setAlias.mutateAsync({ groupId: applyAll ? null : groupId, displayName, avatarUrl });
      setFile(null);
      setPreview(null);
      setClearPicture(false);
      setSaved(true);
    } catch (e) {
      setError(errorMessage(e, t("aliases.saveFailed")));
    } finally {
      setUploading(false);
    }
  };

  return (
    <PageShell title={t("aliases.pageTitle", { group: group?.name ?? "" })} scrollable>
      <PageColumn paneTestId="group-alias-page" data-testid="group-alias-form">
        <p className="text-secondary text-muted">{t("aliases.pageIntro")}</p>
        <TextInput
          id="group-alias-name"
          data-testid="group-alias-name"
          label={t("aliases.nameLabel")}
          value={name}
          onChange={(v) => {
            setName(v);
            setNameError(null);
            setSaved(false);
          }}
          placeholder={t("aliases.namePlaceholder", { name: mainName })}
          error={nameError ?? undefined}
          disabled={busy}
        />

        <div className="flex flex-col gap-2">
          <span className="text-secondary text-dim">{t("aliases.pictureLabel")}</span>
          <div className="flex items-center gap-4">
            <div
              data-testid="group-alias-avatar"
              className="w-14 h-14 overflow-hidden flex items-center justify-center flex-shrink-0 rounded-panel border-2 border-line bg-surface-high"
            >
              {preview ? (
                <img data-testid="group-alias-avatar-new" src={preview} alt="" className="w-full h-full object-cover" />
              ) : shownUrl ? (
                <img data-testid="group-alias-avatar-current" src={shownUrl} alt="" className="w-full h-full object-cover" />
              ) : (
                <User size={22} aria-hidden="true" className="text-muted" />
              )}
            </div>
            <div className="flex flex-col gap-2 items-start">
              <label
                htmlFor="group-alias-avatar-input"
                className="inline-flex items-center gap-1.5 text-secondary font-mono cursor-pointer transition-colors text-accent"
              >
                <Upload size={14} aria-hidden="true" />
                {t("aliases.choosePicture")}
              </label>
              <input
                ref={fileInputRef}
                id="group-alias-avatar-input"
                data-testid="group-alias-avatar-input"
                type="file"
                accept="image/*"
                className="sr-only"
                disabled={busy}
                onChange={(e) => {
                  const f = e.target.files?.[0] ?? null;
                  setFile(f);
                  setClearPicture(false);
                  setSaved(false);
                  setPreview(f && f.type.startsWith("image/") ? URL.createObjectURL(f) : null);
                }}
              />
              {(alias?.avatar_url || file) && !clearPicture && (
                <Button
                  size="sm"
                  variant="ghost"
                  data-testid="group-alias-avatar-clear"
                  disabled={busy}
                  onClick={() => {
                    setFile(null);
                    setPreview(null);
                    setClearPicture(true);
                    setSaved(false);
                    if (fileInputRef.current) {
                      fileInputRef.current.value = "";
                    }
                  }}
                >
                  {t("aliases.mainPicture")}
                </Button>
              )}
            </div>
          </div>
        </div>

        <Switch
          id="group-alias-apply-all"
          data-testid="group-alias-apply-all"
          label={t("aliases.applyAll")}
          description={t("aliases.applyAllHint")}
          checked={applyAll}
          onChange={setApplyAll}
          disabled={busy}
        />

        {error && (
          <p data-testid="group-alias-error" className="text-secondary font-mono text-danger">
            {error}
          </p>
        )}
        {saved && (
          <p data-testid="group-alias-saved" className="text-secondary font-mono text-accent-dim">
            {t("aliases.saved")}
          </p>
        )}

        <div className="flex flex-wrap gap-2">
          <Button
            data-testid="group-alias-save"
            onClick={handleSave}
            disabled={busy}
            isLoading={busy}
            loadingText={t("aliases.saving")}
          >
            {t("aliases.save")}
          </Button>
          {alias && currentUser && (
            <Button
              variant="secondary"
              data-testid="group-alias-use-main"
              disabled={busy || reset.isPending}
              isLoading={reset.isPending}
              loadingText={t("aliases.resetting")}
              onClick={async () => {
                try {
                  await reset.mutateAsync({ groupId, userId: currentUser.id });
                  setName("");
                  setFile(null);
                  setPreview(null);
                  setClearPicture(false);
                  setSaved(true);
                } catch (e) {
                  setError(errorMessage(e, t("aliases.saveFailed")));
                }
              }}
            >
              {t("aliases.useMain")}
            </Button>
          )}
          <Button variant="ghost" data-testid="group-alias-back" onClick={back} disabled={busy}>
            {t("common:actions.back")}
          </Button>
        </div>
      </PageColumn>
    </PageShell>
  );
});
