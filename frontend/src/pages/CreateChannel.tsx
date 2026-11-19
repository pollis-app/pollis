import { errorMessage } from "../utils/errorMessage";
import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { invoke } from "../bridge";
import { useQueryClient } from "@tanstack/react-query";
import { deriveSlug } from "../utils/urlRouting";
import { useNameSlug } from "../hooks/useNameSlug";
import { groupQueryKeys, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { TextInput } from "../components/ui/TextInput";
import { TextArea } from "../components/ui/TextArea";
import { Button } from "../components/ui/Button";
import { Switch } from "../components/ui/Switch";
import type { Channel } from "../types";
import { EmptyState } from "../components/ui/EmptyState";
import { PageColumn } from "../components/ui/PageColumn";
import { FormActions } from "../components/ui/FormActions";

interface CreateChannelProps {
  onSuccess?: (channelId: string, channelType: "text" | "voice") => void;
}

export const CreateChannel: React.FC<CreateChannelProps> = observer(({ onSuccess }) => {
  const { t } = useTranslation("channels");
  const { selectedGroupId, currentUser, addChannel, channels, groups, setSelectedChannelId, newChannelCategoryId } = appStore;
  const queryClient = useQueryClient();
  const { name, slug, onNameChange, onSlugChange, finalSlug: resolveFinalSlug } = useNameSlug();
  const [description, setDescription] = useState("");
  const [channelType, setChannelType] = useState<"text" | "voice">("text");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const currentGroup = groups.find((g) => g.id === selectedGroupId);
  // Created from a category's "+" (#2031): it lands at the bottom of that category.
  const { data: tree } = useUserGroupsWithChannels();
  const category = newChannelCategoryId
    ? tree?.find((g) => g.id === selectedGroupId)?.categories.find((k) => k.id === newChannelCategoryId)
    : undefined;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim()) {
      setError(t("errors.nameRequired"));
      return;
    }
    const finalSlug = resolveFinalSlug();
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
        {
          groupId: selectedGroupId,
          name: name.trim(),
          description: description.trim() || null,
          creatorId: currentUser.id,
          channelType,
          categoryId: category?.id ?? null,
        },
      );
      const channelData: Channel = {
        id: channel.id,
        group_id: channel.group_id,
        slug: finalSlug,
        name: channel.name,
        description: channel.description || '',
        channel_type: channel.channel_type as Channel['channel_type'],
        category_id: category?.id ?? null,
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
      <EmptyState testId="create-channel-no-user">{t("errors.signInRequired")}</EmptyState>
    );
  }

  if (!selectedGroupId || !currentGroup) {
    return (
      <EmptyState
        testId="create-channel-no-group"
        actions={
          <Button
            variant="ghost"
            data-testid="create-channel-go-home-button"
            onClick={() => onSuccess?.("", "text")}
          >
            {t("createChannel.goHome")}
          </Button>
        }
      >
        {t("createChannel.selectGroupFirst")}
      </EmptyState>
    );
  }

  return (
    <PageColumn
      as="form"
      paneTestId="create-channel-page"
      data-testid="create-channel-form"
      onSubmit={handleSubmit}
    >
      {category && (
        <p data-testid="create-channel-category" className="text-secondary text-dim">
          {t("categories.createIn", { name: category.name })}
        </p>
      )}
      <TextInput
        label={t("createChannel.nameLabel")}
        value={name}
        onChange={onNameChange}
        placeholder={t("createChannel.namePlaceholder")}
        disabled={isLoading}
        id="create-channel-name"
        required
      />
      <input data-testid="create-channel-name-input" type="hidden" value={name} readOnly />

      <TextInput
        label={t("createChannel.slugLabel")}
        value={slug}
        onChange={onSlugChange}
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
        <p data-testid="create-channel-error" className="text-secondary font-mono text-danger">
          {error}
        </p>
      )}

      <FormActions>
        <Button
          data-testid="create-channel-submit-button"
          type="submit"
          isLoading={isLoading}
          loadingText={t("createChannel.submitting")}
        >
          {t("createChannel.submit")}
        </Button>
      </FormActions>
    </PageColumn>
  );
});
