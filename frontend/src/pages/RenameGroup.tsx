import { errorMessage } from "../utils/errorMessage";
import React, { useEffect, useState } from "react";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useUpdateGroup, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { TextInput } from "../components/ui/TextInput";
import { TextArea } from "../components/ui/TextArea";
import { Button } from "../components/ui/Button";

interface RenameGroupProps {
  groupId: string;
  onSuccess?: () => void;
}

export const RenameGroup: React.FC<RenameGroupProps> = observer(({ groupId, onSuccess }) => {
  const { currentUser } = appStore;
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const updateGroup = useUpdateGroup();

  const group = groupsWithChannels?.find((g) => g.id === groupId);

  const [name, setName] = useState(group?.name ?? "");
  const [description, setDescription] = useState(group?.description ?? "");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (group) {
      setName(group.name);
      setDescription(group.description ?? "");
    }
  }, [group?.id]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    if (!name.trim()) {
      setError("Name is required");
      return;
    }
    if (!currentUser) {
      setError("User not found");
      return;
    }
    if (!group) {
      setError("Group not found");
      return;
    }
    const trimmedName = name.trim();
    const trimmedDescription = description.trim();
    const nameChanged = trimmedName !== group.name;
    const descriptionChanged = trimmedDescription !== (group.description ?? "");
    if (!nameChanged && !descriptionChanged) {
      onSuccess?.();
      return;
    }
    try {
      await updateGroup.mutateAsync({
        groupId,
        name: nameChanged ? trimmedName : undefined,
        description: descriptionChanged ? trimmedDescription : undefined,
      });
      onSuccess?.();
    } catch (err) {
      setError(errorMessage(err, "Failed to rename group"));
    }
  };

  if (!currentUser) {
    return (
      <div data-testid="rename-group-no-user" className="flex items-center justify-center flex-1" style={{ background: "var(--c-bg)" }}>
        <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>Please sign in</p>
      </div>
    );
  }

  if (!group) {
    return (
      <div data-testid="rename-group-not-found" className="flex items-center justify-center flex-1" style={{ background: "var(--c-bg)" }}>
        <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>Group not found</p>
      </div>
    );
  }

  return (
    <div
      data-testid="rename-group-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: "var(--c-bg)" }}
    >
      <div data-testid="rename-group-content" className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <form
          data-testid="rename-group-form"
          onSubmit={handleSubmit}
          className="w-full max-w-md flex flex-col gap-5"
        >
          <TextInput
            label="Group Name"
            value={name}
            onChange={setName}
            placeholder="My Group"
            disabled={updateGroup.isPending}
            id="rename-group-name"
            required
          />
          <input data-testid="rename-group-name-input" type="hidden" value={name} readOnly />

          <TextArea
            label="Description"
            value={description}
            onChange={setDescription}
            placeholder="Optional description…"
            disabled={updateGroup.isPending}
            rows={2}
            id="rename-group-description"
          />
          <input data-testid="rename-group-description-input" type="hidden" value={description} readOnly />

          {error && (
            <p data-testid="rename-group-error" className="text-xs font-mono" style={{ color: "var(--c-danger)" }}>
              {error}
            </p>
          )}

          <Button
            data-testid="rename-group-submit-button"
            type="submit"
            isLoading={updateGroup.isPending}
            loadingText="Saving…"
            className="w-full"
          >
            Save
          </Button>
        </form>
      </div>
    </div>
  );
});
