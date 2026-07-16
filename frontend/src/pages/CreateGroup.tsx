import { errorMessage } from "../utils/errorMessage";
import React, { useState } from "react";
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
import type { Group } from "../types";

interface CreateGroupProps {
  onSuccess?: (groupId: string) => void;
}

export const CreateGroup: React.FC<CreateGroupProps> = observer(({ onSuccess }) => {
  const { currentUser, addGroup, setSelectedGroupId } = appStore;
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugEdited, setSlugEdited] = useState(false);
  const [description, setDescription] = useState("");
  const [createTextChannel, setCreateTextChannel] = useState(false);
  const [createVoiceChannel, setCreateVoiceChannel] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim()) {
      setError("Name is required");
      return;
    }
    const finalSlug = (slugEdited ? slug : deriveSlug(name)).trim();
    if (!finalSlug) {
      setError("Slug must contain at least one letter or number");
      return;
    }
    if (!currentUser) {
      setError("User not found");
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const group = await invoke<{ id: string; name: string; description?: string; owner_id: string; created_at: string }>(
        'create_group',
        {
          name: name.trim(),
          description: description.trim() || null,
          ownerId: currentUser.id,
          createDefaultTextChannel: createTextChannel,
          createDefaultVoiceChannel: createVoiceChannel,
        },
      );
      const groupData: Group = {
        id: group.id,
        slug: finalSlug,
        name: group.name,
        description: group.description || '',
        created_by: group.owner_id,
        created_at: new Date(group.created_at).getTime(),
        updated_at: new Date(group.created_at).getTime(),
      };
      addGroup(groupData);
      setSelectedGroupId(groupData.id);
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroupsWithChannels(currentUser.id) });
      onSuccess?.(group.id);
    } catch (err) {
      setError(errorMessage(err, "Failed to create group"));
    } finally {
      setIsLoading(false);
    }
  };

  if (!currentUser) {
    return (
      <div data-testid="create-group-no-user" className="flex items-center justify-center flex-1" style={{ background: 'var(--c-bg)' }}>
        <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>Please sign in</p>
      </div>
    );
  }

  return (
    <div
      data-testid="create-group-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: 'var(--c-bg)' }}
    >
      <div data-testid="create-group-content" className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <form
          data-testid="create-group-form"
          onSubmit={handleSubmit}
          className="w-full max-w-md flex flex-col gap-5"
        >
          <TextInput
            label="Group Name"
            value={name}
            onChange={(val) => {
              setName(val);
              if (!slugEdited) { setSlug(deriveSlug(val)); }
            }}
            placeholder="Engineering"
            disabled={isLoading}
            id="create-group-name"
            required
          />
          {/* Preserve testid for E2E */}
          <input data-testid="create-group-name-input" type="hidden" value={name} readOnly />

          <TextInput
            label="Slug"
            value={slug}
            onChange={(val) => { setSlug(val.toLowerCase()); setSlugEdited(true); }}
            placeholder="engineering"
            disabled={isLoading}
            id="create-group-slug"
            required
            description="Auto-generated from name. Letters, numbers, hyphens."
          />
          <input data-testid="create-group-slug-input" type="hidden" value={slug} readOnly />

          <TextArea
            label="Description"
            value={description}
            onChange={setDescription}
            placeholder="Optional description…"
            disabled={isLoading}
            rows={3}
            id="create-group-description"
          />
          <input data-testid="create-group-description-input" type="hidden" value={description} readOnly />

          <Switch
            label="Create a default text channel"
            checked={createTextChannel}
            onChange={setCreateTextChannel}
            disabled={isLoading}
            description="Adds a #General text channel to the new group. You can always add channels later."
          />

          <Switch
            label="Create a default voice channel"
            checked={createVoiceChannel}
            onChange={setCreateVoiceChannel}
            disabled={isLoading}
            description="Adds a Voice Chat channel to the new group. You can always add channels later."
          />

          {error && (
            <p data-testid="create-group-error" className="text-xs font-mono" style={{ color: 'var(--c-danger)' }}>
              {error}
            </p>
          )}

          <Button
            data-testid="create-group-submit-button"
            type="submit"
            isLoading={isLoading}
            loadingText="Creating…"
            className="w-full"
          >
            Create Group
          </Button>
        </form>
      </div>
    </div>
  );
});
