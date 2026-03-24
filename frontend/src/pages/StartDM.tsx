import React, { useState } from "react";
import { useAppStore } from "../stores/appStore";
import { useCreateOrGetDMConversation } from "../hooks/queries";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";

interface StartDMProps {
  onSuccess?: (conversationId: string) => void;
}

export const StartDM: React.FC<StartDMProps> = ({ onSuccess }) => {
  const currentUser = useAppStore((state) => state.currentUser);
  const setSelectedConversationId = useAppStore(
    (state) => state.setSelectedConversationId
  );
  const [identifier, setIdentifier] = useState("");
  const [error, setError] = useState<string | null>(null);

  const createDMMutation = useCreateOrGetDMConversation();

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!identifier.trim()) {
      setError("User identifier is required");
      return;
    }
    if (!currentUser) {
      setError("User not found");
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
        err instanceof Error ? err.message : "Failed to start conversation"
      );
    }
  };

  return (
    <div
      data-testid="start-dm-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: 'var(--c-bg)' }}
    >
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <form
          data-testid="start-dm-form"
          onSubmit={handleSubmit}
          className="w-full max-w-md flex flex-col gap-5"
        >
          <TextInput
            label="Username or Email"
            value={identifier}
            onChange={setIdentifier}
            placeholder="friend@pollis.com"
            disabled={createDMMutation.isPending}
            id="dm-identifier"
            required
          />
          <input data-testid="dm-identifier-input" type="hidden" value={identifier} readOnly />

          {(error || createDMMutation.error) && (
            <p data-testid="start-dm-error" className="text-xs font-mono" style={{ color: '#ff6b6b' }}>
              {error ||
                (createDMMutation.error instanceof Error
                  ? createDMMutation.error.message
                  : "Failed to start conversation")}
            </p>
          )}

          <Button
            data-testid="start-dm-submit-button"
            type="submit"
            isLoading={createDMMutation.isPending}
            loadingText="Starting…"
          >
            Start Conversation
          </Button>
        </form>
      </div>
    </div>
  );
};
