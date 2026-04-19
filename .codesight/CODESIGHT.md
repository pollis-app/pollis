# pollis — AI Context Map

> **Stack:** raw-http | none | react | typescript
> **Monorepo:** frontend

> 0 routes | 30 models | 66 components | 22 lib files | 9 env vars | 3 middleware | 33% test coverage
> **Token savings:** this file is ~5,700 tokens. Without it, AI exploration would cost ~49,900 tokens. **Saves ~44,300 tokens per conversation.**
> **Last scanned:** 2026-04-13 13:50 — re-run after significant changes

---

# Schema

### users_new
- id: text (pk)
- email: text (required)
- username: text (required)
- phone: text
- identity_key: text
- avatar_url: text

### mls_key_package
- ref_hash: text (pk)

### mls_commit_log
- seq: integer (pk)
- conversation_id: text (required, fk)
- epoch: integer (required)
- commit_data: bytes (required)

### mls_welcome
- id: text (pk)
- recipient_id: text (required, fk)
- welcome_data: bytes (required)

### conversation_watermark
- conversation_id: text (required, fk)
- user_id: text (required, fk)
- device_id: text (required, fk)
- last_fetched_at: text (required)

### voice_presence
- user_id: text (required, fk)
- group_id: text (required, fk)
- channel_id: text (required, fk)
- display_name: text (required)
- joined_at: text (required)

### attachment_object
- content_hash: text (pk)
- r2_key: text (required)

### user_device
- device_id: text (pk, fk)
- user_id: text (required, fk)
- device_name: text
- last_seen: text (required)

### account_recovery
- user_id: text (pk, fk)
- identity_version: integer (required)
- salt: bytes (required)
- nonce: bytes (required)
- wrapped_key: bytes (required)

### mls_group_info
- conversation_id: text (pk, fk)
- epoch: integer (required)
- group_info: bytes (required)
- updated_by_device_id: text (required, fk)

### device_enrollment_request
- id: text (pk)
- user_id: text (required, fk)
- new_device_id: text (required, fk)
- new_device_ephemeral_pub: bytes (required)
- verification_code: text (required)
- wrapped_account_key: bytes
- status: text (required)
- expires_at: text (required)
- approved_by_device_id: text (fk)

### security_event
- id: text (pk)
- user_id: text (required, fk)
- kind: text (required)
- device_id: text (fk)
- metadata: text

### kv
- value: text (required)

### identity_key
- id: integer (pk)
- public_key: bytes (required)

### message
- id: text (pk)
- conversation_id: text (required, fk)
- sender_id: text (required, fk)
- ciphertext: bytes (required)
- content: text
- reply_to_id: text (fk)
- sent_at: text (required)
- received_at: text (required)
- delivered: integer (required)
- edited_at: text

### dm_conversation
- id: text (pk)
- peer_user_id: text (required, fk)

### preferences
- preferences: text (required)

### ui_state
- value: text (required)

### mls_kv
- scope: text (required)
- value: bytes (required)

### users
- id: text (pk)
- email: text (required)
- username: text
- phone: text
- identity_key: text
- avatar_url: text

### groups
- id: text (pk)
- name: text (required)
- description: text
- icon_url: text
- owner_id: text (required, fk)

### group_member
- group_id: text (required, fk)
- user_id: text (required, fk)
- role: text (required)
- joined_at: text (required)

### channels
- id: text (pk)
- group_id: text (required, fk)
- name: text (required)
- description: text
- channel_type: text (required)

### message_envelope
- id: text (pk)
- conversation_id: text (required, fk)
- sender_id: text (required, fk)
- ciphertext: text (required)
- reply_to_id: text (fk)
- sent_at: text (required)
- delivered: integer (required)

### dm_channel
- id: text (pk)
- created_by: text (required)

### dm_channel_member
- dm_channel_id: text (required, fk)
- user_id: text (required, fk)
- added_by: text (required)
- added_at: text (required)

### group_invite
- id: text (pk)
- group_id: text (required, fk)
- inviter_id: text (required, fk)
- invitee_id: text (required, fk)

### group_join_request
- id: text (pk)
- group_id: text (required, fk)
- requester_id: text (required, fk)
- reviewed_by: text (fk)
- reviewed_at: text
- status: text (required)

### user_preferences
- user_id: text (pk, fk)
- preferences: text (required)

### message_reaction
- id: text (pk)
- message_id: text (required, fk)
- user_id: text (required, fk)
- emoji: text (required)

---

# Components

- **MainApp** — `frontend/src/App.tsx`
- **EmailOTPAuth** — props: onSuccess, prefillEmail, prefillNonce, onStepChange — `frontend/src/components/Auth/EmailOTPAuth.tsx`
- **EnrollmentApprovalPrompt** — props: requestId, newDeviceId, verificationCode, onResolved — `frontend/src/components/Auth/EnrollmentApprovalPrompt.tsx`
- **EnrollmentGateScreen** — props: userId, userEmail, onEnrolled, onCancel, onResetComplete — `frontend/src/components/Auth/EnrollmentGateScreen.tsx`
- **LoginScreen** — props: knownAccounts, onAuthSuccess, onWipeComplete — `frontend/src/components/Auth/LoginScreen.tsx`
- **SaveSecretKeyScreen** — props: secretKey, onConfirmed — `frontend/src/components/Auth/SaveSecretKeyScreen.tsx`
- **AppShell** — `frontend/src/components/Layout/AppShell.tsx`
- **MainContent** — `frontend/src/components/Layout/MainContent.tsx`
- **PageShell** — props: title, onBack, scrollable — `frontend/src/components/Layout/PageShell.tsx`
- **SidebarActions** — props: isCollapsed, onCreateGroup, onSearchGroup, onHomeClick — `frontend/src/components/Layout/SidebarActions.tsx`
- **SidebarHeader** — props: isCollapsed, onHomeClick — `frontend/src/components/Layout/SidebarHeader.tsx`
- **TitleBar** — `frontend/src/components/Layout/TitleBar.tsx`
- **TreeView** — props: data, className, selectedId, onNodeClick, onNodeAction, getNodeIcon, defaultExpandedIds — `frontend/src/components/Layout/TreeView.tsx`
- **LastMessagePreview** — props: channelId, conversationId — `frontend/src/components/Message/LastMessagePreview.tsx`
- **MessageItem** — props: message, allMessages, authorUsername, isAuthorAdmin, onReply, onEdit, onDelete, onScrollToReply — `frontend/src/components/Message/MessageItem.tsx`
- **MessageList** — props: messages, adminUserIds, onReply, onEdit, onDelete, onScrollToMessage, getAuthorUsername, hasMore, isFetchingMore, onLoadMore — `frontend/src/components/Message/MessageList.tsx`
- **MessageQueue** — `frontend/src/components/Message/MessageQueue.tsx`
- **MessageReactions** — props: messageId — `frontend/src/components/Message/MessageReactions.tsx`
- **ReplyPreview** — props: messageId, allMessages, onDismiss, onScrollToMessage — `frontend/src/components/Message/ReplyPreview.tsx`
- **NetworkStatusIndicator** — `frontend/src/components/NetworkStatusIndicator.tsx`
- **SearchView** — props: onNavigateToConversation — `frontend/src/components/Search/SearchView.tsx`
- **SearchPanel** — props: isOpen, onClose — `frontend/src/components/SearchPanel.tsx`
- **KeyChangeWarning** — props: contactName, oldFingerprint, newFingerprint, onReverify, onContinue, onCancel — `frontend/src/components/Security/KeyChangeWarning.tsx`
- **KeyVerification** — props: contactName, contactId, localFingerprint, remoteFingerprint, keyChanged, onVerified, onCancel — `frontend/src/components/Security/KeyVerification.tsx`
- **SecurityIndicator** — props: kind, label — `frontend/src/components/Security/SecurityIndicator.tsx`
- **SecuritySettings** — props: ownFingerprint, verifiedContacts, sessions, messagePreviewsEnabled, onToggleMessagePreviews, onExportBackup, onImportBackup, onClearSessions, onResetSession — `frontend/src/components/Security/SecuritySettings.tsx`
- **TerminalApp** — props: onLogout, onDeleteAccount — `frontend/src/components/TerminalApp.tsx`
- **UpdateScreen** — `frontend/src/components/UpdateScreen.tsx`
- **VoiceBar** — props: channelId, channelName — `frontend/src/components/Voice/VoiceBar.tsx`
- **VoiceChannelView** — `frontend/src/components/Voice/VoiceChannelView.tsx`
- **DesktopRequiredView** — `frontend/src/features/DesktopRequiredView.tsx`
- **AllJoinRequests** — `frontend/src/pages/AllJoinRequests.tsx`
- **AllJoinRequestsPage** — `frontend/src/pages/AllJoinRequestsPage.tsx`
- **ChannelPage** — `frontend/src/pages/Channel.tsx`
- **CreateChannel** — props: onSuccess — `frontend/src/pages/CreateChannel.tsx`
- **CreateChannelPage** — `frontend/src/pages/CreateChannelPage.tsx`
- **CreateGroup** — props: onSuccess — `frontend/src/pages/CreateGroup.tsx`
- **CreateGroupPage** — `frontend/src/pages/CreateGroupPage.tsx`
- **DMPage** — `frontend/src/pages/DM.tsx`
- **DMSettingsPage** — `frontend/src/pages/DMSettings.tsx`
- **DMsPage** — `frontend/src/pages/DMs.tsx`
- **GroupPage** — `frontend/src/pages/Group.tsx`
- **GroupsPage** — `frontend/src/pages/Groups.tsx`
- **InviteMember** — props: groupId, groupName — `frontend/src/pages/InviteMember.tsx`
- **InviteMemberPage** — `frontend/src/pages/InviteMemberPage.tsx`
- **Invites** — `frontend/src/pages/Invites.tsx`
- **InvitesPage** — `frontend/src/pages/InvitesPage.tsx`
- **JoinRequests** — props: groupId, groupName — `frontend/src/pages/JoinRequests.tsx`
- **JoinRequestsPage** — `frontend/src/pages/JoinRequestsPage.tsx`
- **KickMemberPage** — `frontend/src/pages/KickMemberPage.tsx`
- **LeaveGroupPage** — `frontend/src/pages/LeaveGroup.tsx`
- **Members** — props: groupId, isAdmin — `frontend/src/pages/Members.tsx`
- **MembersPage** — `frontend/src/pages/MembersPage.tsx`
- **Preferences** — `frontend/src/pages/Preferences.tsx`
- **PreferencesPage** — `frontend/src/pages/PreferencesPage.tsx`
- **RootPage** — `frontend/src/pages/Root.tsx`
- **SearchPage** — `frontend/src/pages/Search.tsx`
- **SearchGroup** — `frontend/src/pages/SearchGroup.tsx`
- **SearchGroupPage** — `frontend/src/pages/SearchGroupPage.tsx`
- **SecurityPage** — `frontend/src/pages/SecurityPage.tsx`
- **Settings** — props: onDeleteAccount — `frontend/src/pages/Settings.tsx`
- **SettingsPage** — `frontend/src/pages/SettingsPage.tsx`
- **StartDM** — props: onSuccess — `frontend/src/pages/StartDM.tsx`
- **StartDMPage** — `frontend/src/pages/StartDMPage.tsx`
- **VoiceChannelPage** — `frontend/src/pages/VoiceChannel.tsx`
- **VoiceSettingsPage** — `frontend/src/pages/VoiceSettingsPage.tsx`

---

# Libraries

- `frontend/src/hooks/queries/useGroups.ts`
  - function useUserGroupsWithChannels: () => void
  - function useUserGroups: () => void
  - function useGroupChannels: (groupId) => void
  - function useCreateGroup: () => void
  - function useJoinGroup: () => void
  - function useUpdateGroupIcon: () => void
  - _...18 more_
- `frontend/src/hooks/queries/useMessages.ts`
  - function transformChannelMessage: (m) => Message
  - function useMessages: (channelId, conversationId) => void
  - function useChannelMessages: (channelId) => void
  - function useConversationMessages: (conversationId) => void
  - function useSendMessage: () => void
  - function useDMConversations: () => void
  - _...6 more_
- `frontend/src/hooks/queries/usePreferences.ts`
  - function getPreference: (json, key, defaultValue) => T
  - function usePreferences: () => void
  - function applyPreferences: (prefs) => void
  - interface PreferencesData
- `frontend/src/hooks/queries/useReactions.ts`
  - function useReactions: (messageId) => void
  - function useAddReaction: () => void
  - function useRemoveReaction: () => void
  - const reactionQueryKeys
- `frontend/src/hooks/queries/useSearchMessages.ts` — function useSearchMessages: (query) => void, const searchQueryKeys
- `frontend/src/hooks/queries/useUserProfile.ts`
  - function useUserProfile: () => void
  - function useUpdateProfile: () => void
  - function useUserAvatar: () => void
  - function useUpdateAvatar: () => void
  - interface ServiceUserData
  - const userQueryKeys
- `frontend/src/hooks/queries/useVoiceParticipants.ts` — function useVoiceParticipants: (channelId) => void, function useVoiceRoomCounts: (channelIds) => void
- `frontend/src/hooks/useBadge.ts` — function useBadge: () => void
- `frontend/src/hooks/useLiveKitRealtime.ts` — function useLiveKitRealtime: () => void
- `frontend/src/hooks/useNetworkStatus.ts` — function useNetworkStatus
- `frontend/src/hooks/useTauriReady.ts` — function useTauriReady: () => void, function checkIsDesktop: () => boolean
- `frontend/src/hooks/useVoiceChannel.ts` — function switchVoiceDevice: (kind, deviceName) => void, function useVoiceChannel: (channelId, groupId) => UseVoiceChannelResult
- `frontend/src/hooks/useWindowState.ts` — function restoreWindowState: () => Promise<void>, function useWindowState: () => void
- `frontend/src/services/api.ts`
  - function requestOTP: (email) => Promise<void>
  - function verifyOTP: (email, code) => Promise<AuthResult>
  - function getSession: () => Promise<AuthResult | null>
  - function startDeviceEnrollment: (userId) => Promise<EnrollmentHandle>
  - function pollEnrollmentStatus: (requestId) => Promise<EnrollmentStatus>
  - function listPendingEnrollmentRequests: (userId) => Promise<PendingEnrollmentRequest[]>
  - _...40 more_
- `frontend/src/services/grpc-web-client.ts` — class ServiceClient, const grpcClient
- `frontend/src/services/r2-upload.ts`
  - function uploadAvatar: (userId, _aliasId, file) => Promise<PresignedUploadResponse>
  - function uploadGroupIcon: (groupId, file) => Promise<PresignedUploadResponse>
  - function getFileDownloadUrl: (key) => Promise<string>
  - function downloadAndDecryptMedia: (r2Key, contentHash, mimeType?) => Promise<string>
- `frontend/src/services/web-storage.ts`
  - function initDB: () => Promise<IDBDatabase>
  - function put: (storeName, item) => Promise<void>
  - function get: (storeName, key) => Promise<T | undefined>
  - function getAll: (storeName) => Promise<T[]>
  - function getAllByIndex: (storeName, indexName, key) => Promise<T[]>
  - function remove: (storeName, key) => Promise<void>
  - _...7 more_
- `frontend/src/utils/colorUtils.ts`
  - function hexToHsl: (hex) => [number, number, number]
  - function hslToHex: (h, s, l) => string
  - function applyAccentColor: (hex) => void
  - function applyBackgroundColor: (hex) => void
  - function applyFontSize: (px) => void
  - function readAccentHex: () => string
  - _...2 more_
- `frontend/src/utils/fileIcon.ts` — function getFileIcon: (filename) => LucideIcon
- `frontend/src/utils/imageProcessing.ts`
  - function resizeImage: (file, options) => Promise<File>
  - function blurhashFromUrl: (url) => Promise<
  - function validateImageFile: (file, maxSizeMB) => string | null
  - function generateThumbnail: (file, size) => Promise<File>
  - interface ResizeOptions
- `frontend/src/utils/sfx.ts` — function playSfx: (sound) => void, const SFX
- `frontend/src/utils/urlRouting.ts`
  - function deriveSlug
  - function updateURL
  - function parseURL

---

# Config

## Environment Variables

- `CI` **required** — playwright.config.ts
- `DB_URL` (has default) — .env.example
- `DEV` **required** — frontend/src/App.tsx
- `R2_ACCESS_KEY_ID` (has default) — .env.example
- `R2_PUBLIC_URL` (has default) — .env.example
- `R2_S3_ENDPOINT` (has default) — .env.example
- `R2_SECRET_KEY` (has default) — .env.example
- `VITE_PLAYWRIGHT` **required** — frontend/src/main.tsx
- `VITE_SERVICE_URL` **required** — frontend/src/services/grpc-web-client.ts

## Config Files

- `.env.example`
- `frontend/tailwind.config.js`
- `frontend/vite.config.ts`

---

# Middleware

## custom
- migrate — `src-tauri/src/bin/migrate.rs`

## auth
- auth — `src-tauri/src/commands/auth.rs`
- auth.spec — `tests/e2e/auth.spec.ts`

---

# Dependency Graph

## Most Imported Files (change these carefully)

- `frontend/src/stores/appStore.ts` — imported by **37** files
- `frontend/src/components/ui/Button.tsx` — imported by **23** files
- `frontend/src/hooks/queries/useGroups.ts` — imported by **21** files
- `frontend/src/components/Layout/PageShell.tsx` — imported by **17** files
- `frontend/src/types/index.ts` — imported by **15** files
- `frontend/src/services/api.ts` — imported by **12** files
- `frontend/src/hooks/queries/useMessages.ts` — imported by **11** files
- `frontend/src/components/ui/Card.tsx` — imported by **9** files
- `frontend/src/components/ui/TextInput.tsx` — imported by **9** files
- `frontend/src/hooks/queries/index.ts` — imported by **8** files
- `frontend/src/components/Layout/TitleBar.tsx` — imported by **7** files
- `frontend/src/components/ui/DotMatrix.tsx` — imported by **6** files
- `frontend/src/hooks/queries/usePreferences.ts` — imported by **6** files
- `frontend/src/components/ui/LoaderSpinner.tsx` — imported by **6** files
- `tests/e2e/helpers.ts` — imported by **6** files
- `frontend/src/utils/urlRouting.ts` — imported by **4** files
- `frontend/src/components/ui/Switch.tsx` — imported by **4** files
- `frontend/src/components/ui/TerminalMenu.tsx` — imported by **4** files
- `frontend/src/components/Layout/MainContent.tsx` — imported by **3** files
- `frontend/src/services/r2-upload.ts` — imported by **3** files

## Import Map (who imports what)

- `frontend/src/stores/appStore.ts` ← `frontend/src/App.tsx`, `frontend/src/components/Layout/AppShell.tsx`, `frontend/src/components/Layout/MainContent.tsx`, `frontend/src/components/Message/MessageItem.tsx`, `frontend/src/components/Message/MessageQueue.tsx` +32 more
- `frontend/src/components/ui/Button.tsx` ← `frontend/src/App.tsx`, `frontend/src/components/Auth/EmailOTPAuth.tsx`, `frontend/src/components/Auth/EnrollmentApprovalPrompt.tsx`, `frontend/src/components/Auth/EnrollmentGateScreen.tsx`, `frontend/src/components/Auth/LoginScreen.tsx` +18 more
- `frontend/src/hooks/queries/useGroups.ts` ← `frontend/src/components/Layout/AppShell.tsx`, `frontend/src/components/Layout/MainContent.tsx`, `frontend/src/components/SearchPanel.tsx`, `frontend/src/components/Voice/VoiceBar.tsx`, `frontend/src/hooks/queries/index.ts` +16 more
- `frontend/src/components/Layout/PageShell.tsx` ← `frontend/src/pages/AllJoinRequestsPage.tsx`, `frontend/src/pages/CreateChannelPage.tsx`, `frontend/src/pages/CreateGroupPage.tsx`, `frontend/src/pages/DMSettings.tsx`, `frontend/src/pages/InviteMemberPage.tsx` +12 more
- `frontend/src/types/index.ts` ← `frontend/src/App.tsx`, `frontend/src/components/Auth/LoginScreen.tsx`, `frontend/src/components/Layout/MainContent.tsx`, `frontend/src/components/Message/MessageItem.tsx`, `frontend/src/components/Message/MessageList.tsx` +10 more
- `frontend/src/services/api.ts` ← `frontend/src/App.tsx`, `frontend/src/components/Auth/EmailOTPAuth.tsx`, `frontend/src/components/Auth/EnrollmentApprovalPrompt.tsx`, `frontend/src/components/Auth/EnrollmentGateScreen.tsx`, `frontend/src/components/Auth/LoginScreen.tsx` +7 more
- `frontend/src/hooks/queries/useMessages.ts` ← `frontend/src/components/Layout/AppShell.tsx`, `frontend/src/components/Layout/MainContent.tsx`, `frontend/src/components/Message/LastMessagePreview.tsx`, `frontend/src/components/SearchPanel.tsx`, `frontend/src/hooks/queries/index.ts` +6 more
- `frontend/src/components/ui/Card.tsx` ← `frontend/src/App.tsx`, `frontend/src/components/Auth/EnrollmentApprovalPrompt.tsx`, `frontend/src/components/Auth/EnrollmentGateScreen.tsx`, `frontend/src/components/Auth/LoginScreen.tsx`, `frontend/src/components/Auth/SaveSecretKeyScreen.tsx` +4 more
- `frontend/src/components/ui/TextInput.tsx` ← `frontend/src/components/Auth/EmailOTPAuth.tsx`, `frontend/src/components/Auth/EnrollmentGateScreen.tsx`, `frontend/src/components/Auth/SaveSecretKeyScreen.tsx`, `frontend/src/pages/CreateChannel.tsx`, `frontend/src/pages/CreateGroup.tsx` +4 more
- `frontend/src/hooks/queries/index.ts` ← `frontend/src/components/Layout/MainContent.tsx`, `frontend/src/pages/AllJoinRequests.tsx`, `frontend/src/pages/InviteMember.tsx`, `frontend/src/pages/Invites.tsx`, `frontend/src/pages/JoinRequests.tsx` +3 more

---

# Test Coverage

> **33%** of routes and models are covered by tests
> 8 test files found

## Covered Models

- mls_key_package
- mls_welcome
- kv
- message
- preferences
- mls_kv
- users
- groups
- channels
- group_invite

---

_Generated by [codesight](https://github.com/Houseofmvps/codesight) — see your codebase clearly_