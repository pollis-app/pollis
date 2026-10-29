import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";

/** The Vault (#107): sealed under the account key in Rust; a new device decrypts all of it. */

/** Mirrors the Rust VaultMessage in pollis-core/src/commands/vault.rs. */
export interface VaultMessage {
  id: string;
  /** Chat content-string shape; parse with `parseContent`. */
  content: string;
  pinned: boolean;
  created_at: string;
  updated_at: string;
}

export const vaultQueryKeys = {
  all: ["vault"] as const,
};

/** Every entry, oldest first; Rust syncs or serves the cache. */
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

/** Add one entry built by `buildMessageContent`. */
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

/** Pin/unpin; looks like an edit on the wire. */
export function useSetVaultMessagePinned() {
  return useVaultMutation<{ id: string; pinned: boolean }>((vars, userId) =>
    invoke("set_vault_message_pinned", {
      id: vars.id,
      pinned: vars.pinned,
      userId,
    }),
  );
}
