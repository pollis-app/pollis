// The Vault (#1316), mirroring desktop's useVault.ts. A new device decrypts the whole vault at enrollment.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { buildMessageContent, type PickedAttachment } from "../../lib/attachments";
import { appStore } from "../../stores/appStore";
import type { VaultMessage } from "../../lib/vault";

export type { VaultMessage } from "../../lib/vault";

export const vaultQueryKeys = {
  all: ["vault"] as const,
};

/** Every entry, oldest first. `active: false` stops a screen that only sometimes shows it from syncing. */
export function useVaultMessages(active = true) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: vaultQueryKeys.all,
    queryFn: async (): Promise<VaultMessage[]> =>
      (await invoke<VaultMessage[]>("get_vault_messages", {
        userId: currentUser!.id,
      })) ?? [],
    enabled: active && Boolean(currentUser),
    staleTime: 1000 * 30,
  });
}

function useVaultUserId(): string | null {
  return useObserver(() => appStore.currentUser?.id ?? null);
}

function requireUser(userId: string | null): string {
  if (!userId) {
    throw new Error("Not signed in");
  }
  return userId;
}

/** Files ride the chat's upload pipeline and envelope. */
export function useSendVaultMessage() {
  const queryClient = useQueryClient();
  const userId = useVaultUserId();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { mutate: "send" } },
    mutationFn: async (vars: { text: string; attachments: PickedAttachment[] }) => {
      const uid = requireUser(userId);
      const content = await buildMessageContent(vars.attachments, vars.text);
      return await invoke<VaultMessage>("send_vault_message", { userId: uid, content });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: vaultQueryKeys.all });
    },
  });
}

export function useEditVaultMessage() {
  const queryClient = useQueryClient();
  const userId = useVaultUserId();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { id: string; newContent: string }) =>
      await invoke<VaultMessage>("edit_vault_message", {
        id: vars.id,
        newContent: vars.newContent,
        userId: requireUser(userId),
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: vaultQueryKeys.all });
    },
  });
}

export function useDeleteVaultMessage() {
  const queryClient = useQueryClient();
  const userId = useVaultUserId();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (id: string) => {
      await invoke("delete_vault_message", { id, userId: requireUser(userId) });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: vaultQueryKeys.all });
    },
  });
}

/** Indistinguishable from an edit on the wire. */
export function useSetVaultMessagePinned() {
  const queryClient = useQueryClient();
  const userId = useVaultUserId();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "toggle" } },
    mutationFn: async (vars: { id: string; pinned: boolean }) =>
      await invoke<VaultMessage>("set_vault_message_pinned", {
        id: vars.id,
        pinned: vars.pinned,
        userId: requireUser(userId),
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: vaultQueryKeys.all });
    },
  });
}
