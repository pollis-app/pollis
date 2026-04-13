# Libraries

> **Navigation aid.** Library inventory extracted via AST. Read the source files listed here before modifying exported functions.

**22 library files** across 1 module

## Frontend (22 files)

- `frontend/src/services/api.ts` — requestOTP, verifyOTP, getSession, startDeviceEnrollment, pollEnrollmentStatus, listPendingEnrollmentRequests, …
- `frontend/src/hooks/queries/useGroups.ts` — useUserGroupsWithChannels, useUserGroups, useGroupChannels, useCreateGroup, useJoinGroup, useUpdateGroupIcon, …
- `frontend/src/services/web-storage.ts` — initDB, put, get, getAll, getAllByIndex, remove, …
- `frontend/src/hooks/queries/useMessages.ts` — transformChannelMessage, useMessages, useChannelMessages, useConversationMessages, useSendMessage, useDMConversations, …
- `frontend/src/utils/colorUtils.ts` — hexToHsl, hslToHex, applyAccentColor, applyBackgroundColor, applyFontSize, readAccentHex, …
- `frontend/src/hooks/queries/useUserProfile.ts` — useUserProfile, useUpdateProfile, useUserAvatar, useUpdateAvatar, ServiceUserData, userQueryKeys
- `frontend/src/utils/imageProcessing.ts` — resizeImage, blurhashFromUrl, validateImageFile, generateThumbnail, ResizeOptions
- `frontend/src/hooks/queries/usePreferences.ts` — getPreference, usePreferences, applyPreferences, PreferencesData
- `frontend/src/hooks/queries/useReactions.ts` — useReactions, useAddReaction, useRemoveReaction, reactionQueryKeys
- `frontend/src/services/grpc-web-client.ts` — ServiceClient, grpcClient _(legacy, may be unused)_
- `frontend/src/services/r2-upload.ts` — uploadAvatar, uploadGroupIcon, getFileDownloadUrl, downloadAndDecryptMedia
- `frontend/src/utils/urlRouting.ts` — deriveSlug, updateURL, parseURL
- `frontend/src/hooks/queries/useSearchMessages.ts` — useSearchMessages, searchQueryKeys
- `frontend/src/hooks/queries/useVoiceParticipants.ts` — useVoiceParticipants, useVoiceRoomCounts
- `frontend/src/hooks/useTauriReady.ts` — useTauriReady, checkIsDesktop
- `frontend/src/hooks/useVoiceChannel.ts` — switchVoiceDevice, useVoiceChannel
- `frontend/src/hooks/useWindowState.ts` — restoreWindowState, useWindowState
- `frontend/src/utils/sfx.ts` — playSfx, SFX
- `frontend/src/hooks/useBadge.ts` — useBadge
- `frontend/src/hooks/useLiveKitRealtime.ts` — useLiveKitRealtime
- `frontend/src/hooks/useNetworkStatus.ts` — useNetworkStatus
- `frontend/src/utils/fileIcon.ts` — getFileIcon

---
**Note:** This only covers frontend libraries. The Rust backend (`src-tauri/src/commands/`) is documented in [commands.md](./commands.md).

_Back to [index.md](./index.md)_