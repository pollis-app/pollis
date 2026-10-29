import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";

/**
 * The Vault (#107): a personal encrypted space synced across the account's
 * devices. Rust seals entries under a key derived from the account identity
 * key, so a new device decrypts the whole vault at enrollment.
 */

export interface VaultMessage {
  id: string;
  /** The chat content-string shape; parse with `parseContent`. */
  content: string;
  pinned: boolean;
  created_at: string;
  updated_at: string;
}

export const vaultQueryKeys = {
  all: ["vault"] as const,
};

/** Every entry, oldest first; Rust syncs from the DS or serves the local cache. */
export function useVaultMessages() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: vaultQueryKeys.all,
    queryFn: async (): Promise<VaultMessage[]> =>
      (await invoke<VaultMessage[]>("get_vault_messages", {
        userId: currentUser!.id,
      })) ?? [],
    enabled: Boolean(currentUser),
    staleTime: 1000 * 30,
  });
}

function useVaultMutation<TVars>(
  run: (vars: TVars, userId: string) => Promise<unknown>,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (vars: TVars) => {
      if (!currentUser) {
        throw new Error("Not signed in");
      }
      await run(vars, currentUser.id);
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: vaultQueryKeys.all });
    },
  });
}

/** Add one entry; `content` comes from `buildMessageContent`, like chat attachments. */
export function useSendVaultMessage() {
  return useVaultMutation<{ content: string }>((vars, userId) =>
    invoke("send_vault_message", { userId, content: vars.content }),
  );
}

export function useEditVaultMessage() {
  return useVaultMutation<{ id: string; newContent: string }>((vars, userId) =>
    invoke("edit_vault_message", {
      id: vars.id,
      newContent: vars.newContent,
      userId,
    }),
  );
}

export function useDeleteVaultMessage() {
  return useVaultMutation<{ id: string }>((vars, userId) =>
    invoke("delete_vault_message", { id: vars.id, userId }),
  );
}

/** Pin/unpin an entry; on the wire indistinguishable from an edit. */
export function useSetVaultMessagePinned() {
  return useVaultMutation<{ id: string; pinned: boolean }>((vars, userId) =>
    invoke("set_vault_message_pinned", {
      id: vars.id,
      pinned: vars.pinned,
      userId,
    }),
  );
}
