import { errorMessage } from "../utils/errorMessage";
import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useUpdateChannel, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { TextInput } from "../components/ui/TextInput";
import { TextArea } from "../components/ui/TextArea";
import { Button } from "../components/ui/Button";

interface RenameChannelProps {
  groupId: string;
  channelId: string;
  onSuccess?: () => void;
}

export const RenameChannel: React.FC<RenameChannelProps> = observer(({ groupId, channelId, onSuccess }) => {
  const { t } = useTranslation("channels");
  const { currentUser } = appStore;
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const updateChannel = useUpdateChannel();

  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);

  const [name, setName] = useState(channel?.name ?? "");
  const [description, setDescription] = useState(channel?.description ?? "");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (channel) {
      setName(channel.name);
      setDescription(channel.description ?? "");
    }
  }, [channel?.id]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    if (!name.trim()) {
      setError(t("errors.nameRequired"));
      return;
    }
    if (!currentUser) {
      setError(t("errors.userNotFound"));
      return;
    }
    if (!channel) {
      setError(t("renameChannel.notFound"));
      return;
    }
    const trimmedName = name.trim();
    const trimmedDescription = description.trim();
    const nameChanged = trimmedName !== channel.name;
    const descriptionChanged = trimmedDescription !== (channel.description ?? "");
    if (!nameChanged && !descriptionChanged) {
      onSuccess?.();
      return;
    }
    try {
      await updateChannel.mutateAsync({
        groupId,
        channelId,
        name: nameChanged ? trimmedName : undefined,
        description: descriptionChanged ? trimmedDescription : undefined,
      });
      onSuccess?.();
    } catch (err) {
      setError(errorMessage(err, t("renameChannel.renameFailed")));
    }
  };

  if (!currentUser) {
    return (
      <div data-testid="rename-channel-no-user" className="flex items-center justify-center flex-1" style={{ background: "var(--c-bg)" }}>
        <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>{t("errors.signInRequired")}</p>
      </div>
    );
  }

  if (!channel) {
    return (
      <div data-testid="rename-channel-not-found" className="flex items-center justify-center flex-1" style={{ background: "var(--c-bg)" }}>
        <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>{t("renameChannel.notFound")}</p>
      </div>
    );
  }

  return (
    <div
      data-testid="rename-channel-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: "var(--c-bg)" }}
    >
      <div data-testid="rename-channel-content" className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <form
          data-testid="rename-channel-form"
          onSubmit={handleSubmit}
          className="w-full max-w-md flex flex-col gap-5"
        >
          <TextInput
            label={t("renameChannel.nameLabel")}
            value={name}
            onChange={setName}
            placeholder={t("renameChannel.namePlaceholder")}
            disabled={updateChannel.isPending}
            id="rename-channel-name"
            required
          />
          <input data-testid="rename-channel-name-input" type="hidden" value={name} readOnly />

          <TextArea
            label={t("renameChannel.descriptionLabel")}
            value={description}
            onChange={setDescription}
            placeholder={t("renameChannel.descriptionPlaceholder")}
            disabled={updateChannel.isPending}
            rows={2}
            id="rename-channel-description"
          />
          <input data-testid="rename-channel-description-input" type="hidden" value={description} readOnly />

          {error && (
            <p data-testid="rename-channel-error" className="text-xs font-mono" style={{ color: "var(--c-danger)" }}>
              {error}
            </p>
          )}

          <Button
            data-testid="rename-channel-submit-button"
            type="submit"
            isLoading={updateChannel.isPending}
            loadingText={t("renameChannel.submitting")}
            className="w-full"
          >
            {t("renameChannel.submit")}
          </Button>
        </form>
      </div>
    </div>
  );
});
