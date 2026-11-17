import { errorMessage } from "../utils/errorMessage";
import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useCreateOrGetDMConversation } from "../hooks/queries";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";
import { PageColumn } from "../components/ui/PageColumn";
import { FormActions } from "../components/ui/FormActions";

interface StartDMProps {
  onSuccess?: (conversationId: string) => void;
}

export const StartDM: React.FC<StartDMProps> = observer(({ onSuccess }) => {
  const { t } = useTranslation("dms");
  const currentUser = appStore.currentUser;
  const setSelectedConversationId = appStore.setSelectedConversationId;
  const [identifier, setIdentifier] = useState("");
  const [error, setError] = useState<string | null>(null);

  const createDMMutation = useCreateOrGetDMConversation();

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!identifier.trim()) {
      setError(t("start.identifierRequired"));
      return;
    }
    if (!currentUser) {
      setError(t("start.userNotFound"));
      return;
    }
    setError(null);
    try {
      const conversation = await createDMMutation.mutateAsync(identifier.trim());
      setSelectedConversationId(conversation.id);
      setIdentifier("");
      onSuccess?.(conversation.id);
    } catch (err) {
      setError(
        errorMessage(err, t("start.startFailed"))
      );
    }
  };

  return (
    <PageColumn
      as="form"
      paneTestId="start-dm-page"
      data-testid="start-dm-form"
      onSubmit={handleSubmit}
    >
      <TextInput
        label={t("start.identifierLabel")}
        value={identifier}
        onChange={setIdentifier}
        placeholder={t("start.identifierPlaceholder")}
        disabled={createDMMutation.isPending}
        id="dm-identifier"
        required
      />
      <input data-testid="dm-identifier-input" type="hidden" value={identifier} readOnly />

      {(error || createDMMutation.error) && (
        <p data-testid="start-dm-error" className="text-secondary font-mono text-danger">
          {error ||
            (createDMMutation.error instanceof Error
              ? createDMMutation.error.message
              : t("start.startFailed"))}
        </p>
      )}

      <FormActions>
        <Button
          data-testid="start-dm-submit-button"
          type="submit"
          disabled={!identifier.trim()}
          isLoading={createDMMutation.isPending}
          loadingText={t("start.submitting")}
        >
          {t("start.submit")}
        </Button>
      </FormActions>
    </PageColumn>
  );
});
