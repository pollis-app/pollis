import { errorMessage } from "../utils/errorMessage";
import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import {
  useUpdateChannel,
  useUpdateGroup,
  useUserGroupsWithChannels,
} from "../hooks/queries/useGroups";
import { TextInput } from "../components/ui/TextInput";
import { TextArea } from "../components/ui/TextArea";
import { Button } from "../components/ui/Button";
import { EmptyState } from "../components/ui/EmptyState";

type RenameEntityProps =
  | { kind: "group"; groupId: string; channelId?: undefined; onSuccess?: () => void }
  | { kind: "channel"; groupId: string; channelId: string; onSuccess?: () => void };

/**
 * Rename-and-describe form for a group or a channel (#874).
 *
 * These were two components, `RenameGroup` and `RenameChannel`, eleven
 * normalised lines apart: identical local state, identical trim/compare/
 * skip-if-unchanged submit, identical two signed-out and not-found guards,
 * identical markup. Everything that actually differed is the four lines below
 * — which entity is looked up, which mutation saves it, what the copy says,
 * and what the testids are called.
 *
 * The copy is spelled out as literal `t()` calls per kind rather than
 * assembled from a key prefix: `scripts/i18n-check.mjs` resolves literal call
 * sites only, and a computed key is invisible to it.
 */
export const RenameEntity: React.FC<RenameEntityProps> = observer((props) => {
  const { kind, groupId, onSuccess } = props;
  const { t } = useTranslation("channels");
  const { currentUser } = appStore;
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  // Both mutation hooks unconditionally — they register no queries and fire
  // nothing until called, and hooks may not sit behind a branch.
  const updateGroup = useUpdateGroup();
  const updateChannel = useUpdateChannel();

  const isGroup = kind === "group";
  const testId = isGroup ? "rename-group" : "rename-channel";
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = isGroup
    ? undefined
    : group?.channels.find((c) => c.id === props.channelId);
  const entity = isGroup ? group : channel;
  const isPending = isGroup ? updateGroup.isPending : updateChannel.isPending;

  const copy = isGroup
    ? {
        nameLabel: t("renameGroup.nameLabel"),
        namePlaceholder: t("renameGroup.namePlaceholder"),
        descriptionLabel: t("renameGroup.descriptionLabel"),
        descriptionPlaceholder: t("renameGroup.descriptionPlaceholder"),
        submit: t("renameGroup.submit"),
        submitting: t("renameGroup.submitting"),
        notFound: t("renameGroup.notFound"),
        renameFailed: t("renameGroup.renameFailed"),
      }
    : {
        nameLabel: t("renameChannel.nameLabel"),
        namePlaceholder: t("renameChannel.namePlaceholder"),
        descriptionLabel: t("renameChannel.descriptionLabel"),
        descriptionPlaceholder: t("renameChannel.descriptionPlaceholder"),
        submit: t("renameChannel.submit"),
        submitting: t("renameChannel.submitting"),
        notFound: t("renameChannel.notFound"),
        renameFailed: t("renameChannel.renameFailed"),
      };

  const [name, setName] = useState(entity?.name ?? "");
  const [description, setDescription] = useState(entity?.description ?? "");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (entity) {
      setName(entity.name);
      setDescription(entity.description ?? "");
    }
  }, [entity?.id]);

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
    if (!entity) {
      setError(copy.notFound);
      return;
    }
    const trimmedName = name.trim();
    const trimmedDescription = description.trim();
    const nameChanged = trimmedName !== entity.name;
    const descriptionChanged = trimmedDescription !== (entity.description ?? "");
    if (!nameChanged && !descriptionChanged) {
      onSuccess?.();
      return;
    }
    try {
      if (props.kind === "group") {
        await updateGroup.mutateAsync({
          groupId,
          name: nameChanged ? trimmedName : undefined,
          description: descriptionChanged ? trimmedDescription : undefined,
        });
      } else {
        await updateChannel.mutateAsync({
          groupId,
          channelId: props.channelId,
          name: nameChanged ? trimmedName : undefined,
          description: descriptionChanged ? trimmedDescription : undefined,
        });
      }
      onSuccess?.();
    } catch (err) {
      setError(errorMessage(err, copy.renameFailed));
    }
  };

  if (!currentUser) {
    return (
      <EmptyState testId={`${testId}-no-user`}>
        {t("errors.signInRequired")}
      </EmptyState>
    );
  }

  if (!entity) {
    return (
      <EmptyState testId={`${testId}-not-found`}>{copy.notFound}</EmptyState>
    );
  }

  return (
    <div
      data-testid={`${testId}-page`}
      className="flex-1 flex flex-col overflow-auto bg-bg"
    >
      <div
        data-testid={`${testId}-content`}
        className="flex-1 flex justify-center overflow-auto px-6 py-8"
      >
        <form
          data-testid={`${testId}-form`}
          onSubmit={handleSubmit}
          className="w-full max-w-md flex flex-col gap-5"
        >
          <TextInput
            label={copy.nameLabel}
            value={name}
            onChange={setName}
            placeholder={copy.namePlaceholder}
            disabled={isPending}
            id={`${testId}-name`}
            required
          />
          <input
            data-testid={`${testId}-name-input`}
            type="hidden"
            value={name}
            readOnly
          />

          <TextArea
            label={copy.descriptionLabel}
            value={description}
            onChange={setDescription}
            placeholder={copy.descriptionPlaceholder}
            disabled={isPending}
            rows={2}
            id={`${testId}-description`}
          />
          <input
            data-testid={`${testId}-description-input`}
            type="hidden"
            value={description}
            readOnly
          />

          {error && (
            <p
              data-testid={`${testId}-error`}
              className="text-xs font-mono text-danger"
            >
              {error}
            </p>
          )}

          <Button
            data-testid={`${testId}-submit-button`}
            type="submit"
            isLoading={isPending}
            loadingText={copy.submitting}
            className="w-full"
          >
            {copy.submit}
          </Button>
        </form>
      </div>
    </div>
  );
});
