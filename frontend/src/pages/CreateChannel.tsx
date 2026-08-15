import { errorMessage } from "../utils/errorMessage";
import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { invoke } from "../bridge";
import { useQueryClient } from "@tanstack/react-query";
import { deriveSlug } from "../utils/urlRouting";
import { groupQueryKeys } from "../hooks/queries/useGroups";
import { TextInput } from "../components/ui/TextInput";
import { TextArea } from "../components/ui/TextArea";
import { Button } from "../components/ui/Button";
import { Switch } from "../components/ui/Switch";
import type { Channel } from "../types";

interface CreateChannelProps {
  onSuccess?: (channelId: string, channelType: "text" | "voice") => void;
}

export const CreateChannel: React.FC<CreateChannelProps> = observer(({ onSuccess }) => {
  const { t } = useTranslation("channels");
  const { selectedGroupId, currentUser, addChannel, channels, groups, setSelectedChannelId } = appStore;
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugEdited, setSlugEdited] = useState(false);
  const [description, setDescription] = useState("");
  const [channelType, setChannelType] = useState<"text" | "voice">("text");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const currentGroup = groups.find((g) => g.id === selectedGroupId);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim()) {
      setError(t("errors.nameRequired"));
      return;
    }
    const finalSlug = (slugEdited ? slug : deriveSlug(name)).trim();
    if (!finalSlug) {
      setError(t("createChannel.slugRequired"));
      return;
    }
    const groupChannels = selectedGroupId ? channels[selectedGroupId] || [] : [];
    const channelSlugLower = finalSlug.toLowerCase();
    const duplicateExists = groupChannels.some((ch) => {
      const existingSlug = ch.slug?.toLowerCase() ?? deriveSlug(ch.name).toLowerCase();
      return existingSlug === channelSlugLower;
    });
    if (duplicateExists) {
      setError(t("createChannel.duplicate", { slug: finalSlug }));
      return;
    }
    if (!currentUser) {
      setError(t("errors.userNotFound"));
      return;
    }
    if (!selectedGroupId) {
      setError(t("createChannel.noGroupSelected"));
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const channel = await invoke<{ id: string; group_id: string; name: string; description?: string; channel_type: string }>(
        'create_channel',
        { groupId: selectedGroupId, name: name.trim(), description: description.trim() || null, creatorId: currentUser.id, channelType },
      );
      const channelData: Channel = {
        id: channel.id,
        group_id: channel.group_id,
        slug: finalSlug,
        name: channel.name,
        description: channel.description || '',
        channel_type: channel.channel_type as Channel['channel_type'],
        created_by: currentUser.id,
        created_at: Date.now(),
        updated_at: Date.now(),
      };
      addChannel(channelData);
      setSelectedChannelId(channelData.id);
      // Invalidate both channel queries so the sidebar and group page reflect the new channel
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroupsWithChannels(currentUser.id) }),
        queryClient.invalidateQueries({ queryKey: groupQueryKeys.channels(selectedGroupId) }),
      ]);
      onSuccess?.(channel.id, channel.channel_type as "text" | "voice");
    } catch (err) {
      setError(errorMessage(err, t("createChannel.createFailed")));
    } finally {
      setIsLoading(false);
    }
  };

  if (!currentUser) {
    return (
      <div data-testid="create-channel-no-user" className="flex items-center justify-center flex-1" style={{ background: 'var(--c-bg)' }}>
        <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>{t("errors.signInRequired")}</p>
      </div>
    );
  }

  if (!selectedGroupId || !currentGroup) {
    return (
      <div data-testid="create-channel-no-group" className="flex flex-col items-center justify-center flex-1 gap-3" style={{ background: 'var(--c-bg)' }}>
        <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>{t("createChannel.selectGroupFirst")}</p>
        <button
          data-testid="create-channel-go-home-button"
          onClick={() => onSuccess?.("", "text")}
          className="text-xs font-mono transition-colors text-[var(--c-text-muted)] hover:text-[var(--c-accent)]"
        >
          {t("createChannel.goHome")}
        </button>
      </div>
    );
  }

  return (
    <div
      data-testid="create-channel-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: 'var(--c-bg)' }}
    >
      <div data-testid="create-channel-content" className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <form
          data-testid="create-channel-form"
          onSubmit={handleSubmit}
          className="w-full max-w-md flex flex-col gap-5"
        >
          <TextInput
            label={t("createChannel.nameLabel")}
            value={name}
            onChange={(val) => {
              setName(val);
              if (!slugEdited) { setSlug(deriveSlug(val)); }
            }}
            placeholder={t("createChannel.namePlaceholder")}
            disabled={isLoading}
            id="create-channel-name"
            required
          />
          <input data-testid="create-channel-name-input" type="hidden" value={name} readOnly />

          <TextInput
            label={t("createChannel.slugLabel")}
            value={slug}
            onChange={(val) => { setSlug(val.toLowerCase()); setSlugEdited(true); }}
            placeholder={t("createChannel.slugPlaceholder")}
            disabled={isLoading}
            id="create-channel-slug"
            required
          />
          <input data-testid="create-channel-slug-input" type="hidden" value={slug} readOnly />

          <TextArea
            label={t("createChannel.descriptionLabel")}
            value={description}
            onChange={setDescription}
            placeholder={t("createChannel.descriptionPlaceholder")}
            disabled={isLoading}
            rows={2}
            id="create-channel-description"
          />
          <input data-testid="create-channel-description-input" type="hidden" value={description} readOnly />

          <Switch
            label={t("createChannel.voiceLabel")}
            checked={channelType === "voice"}
            onChange={(checked) => setChannelType(checked ? "voice" : "text")}
            disabled={isLoading}
            id="create-channel-type"
            description={t("createChannel.voiceDescription")}
          />
          <input data-testid="create-channel-type-input" type="hidden" value={channelType} readOnly />

          {error && (
            <p data-testid="create-channel-error" className="text-xs font-mono" style={{ color: 'var(--c-danger)' }}>
              {error}
            </p>
          )}

          <Button
            data-testid="create-channel-submit-button"
            type="submit"
            isLoading={isLoading}
            loadingText={t("createChannel.submitting")}
            className="w-full"
          >
            {t("createChannel.submit")}
          </Button>
        </form>
      </div>
    </div>
  );
});
