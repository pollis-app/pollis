// The Vault (#107, mobile #1316): a personal encrypted space that syncs
// across the account's devices like cloud storage. Mirrors desktop's
// frontend/src/hooks/queries/useVault.ts over the same pollis-core commands
// (pollis-core/src/commands/vault.rs), reached through the bridge arms in
// pollis-core/src/bridge.rs.
//
// Every command round-trips through Rust, which seals/opens entries under a
// key derived from the account identity key — the DS stores ciphertext only,
// and a brand-new device decrypts the whole vault at enrollment (the
// deliberate inversion of "a new device starts empty").

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

/** Every vault entry, oldest first (chat order). Syncs from the DS when
 *  reachable and serves the local cache offline — the Rust command decides.
 *  `active: false` keeps a screen that only sometimes shows the vault (the
 *  media viewer) from syncing it. */
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

/** Add one entry. Picked files ride the same upload pipeline as chat
 *  attachments (`buildMessageContent`), so the envelope is the chat's. */
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

/** Pin/unpin an entry — on the wire indistinguishable from an edit, so the
 *  server never learns which entries matter most. */
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
