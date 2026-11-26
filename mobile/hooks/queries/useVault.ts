// The Vault (#1316), mirroring desktop's useVault.ts. A new device decrypts the whole vault at enrollment.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { outgoingAttachments, type PickedAttachment } from "../../lib/attachments";
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

/**
 * Save through the Vault's durable queue (#2100), as a message goes through the outbox: the files
 * go to the core un-uploaded, under one id per submit, so a repeat saves once and an interrupted
 * save is finished by the next drain. A save the core stored but could not finish yet resolves:
 * its row reads "Not saved" with Retry, and the draft is not handed back (that would save twice).
 */
export function useSendVaultMessage() {
  const queryClient = useQueryClient();
  const userId = useVaultUserId();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { mutate: "send" } },
    mutationFn: async (vars: { text: string; attachments: PickedAttachment[] }) => {
      const uid = requireUser(userId);
      const messageId = await invoke<string>("new_message_id");
      try {
        return await invoke<VaultMessage>("send_vault_message", {
          userId: uid,
          content: vars.text,
          messageId,
          attachments: outgoingAttachments(vars.attachments),
        });
      } catch (e) {
        const held = await invoke<VaultMessage[]>("get_vault_messages", { userId: uid }).catch(
          () => [] as VaultMessage[],
        );
        const queued = held.find((entry) => entry.id === messageId);
        if (queued) {
          return queued;
        }
        throw e;
      }
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: vaultQueryKeys.all });
    },
  });
}

/** Retry a queued save that reads "Not saved". */
export function useRetryVaultMessage() {
  const queryClient = useQueryClient();
  const userId = useVaultUserId();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { mutate: "send" } },
    mutationFn: async (id: string) =>
      await invoke<VaultMessage>("retry_vault_message", { id, userId: requireUser(userId) }),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: vaultQueryKeys.all });
    },
  });
}

/** Drop a queued save that was never saved. */
export function useDiscardUnsentVaultMessage() {
  const queryClient = useQueryClient();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (id: string) => await invoke<boolean>("discard_unsent_vault_message", { id }),
    onSettled: () => {
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
