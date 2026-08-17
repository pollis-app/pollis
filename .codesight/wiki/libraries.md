# Libraries

> **Navigation aid, regenerated — not hand-maintained.** Read the source files listed
> here before modifying exported functions. This inventory drifted badly once
> (it named four files that had been deleted: `services/web-storage.ts`,
> `services/grpc-web-client.ts`, `hooks/useVoiceChannel.ts`,
> `hooks/useNetworkStatus.ts` — #804), so treat the filesystem as the authority and
> regenerate rather than patch when it disagrees.

**36 library files** under `frontend/src/{services,utils,hooks}`

## Frontend (36 files)

- `frontend/src/services/api.ts` — requestOTP, AuthResult, verifyOTP, UnlockStateSnapshot, getUnlockState, setPin, …
- `frontend/src/services/r2-upload.ts` — uploadAvatar, uploadGroupIcon, getFileDownloadUrl, downloadAndDecryptMedia, getMediaUrl
- `frontend/src/services/updatePoller.ts` — startUpdatePolling, stopUpdatePolling
- `frontend/src/utils/colorUtils.ts` — hexToHsl, hslToHex, applyAccentColor, applyBackgroundColor, applyFontSize, Skin, …
- `frontend/src/utils/drafts.ts` — getDraft, setDraft, clearAllDrafts
- `frontend/src/utils/errorMessage.ts` — errorMessage
- `frontend/src/utils/fileIcon.ts` — EXT_MIME, IMAGE_EXT_MIME, mimeFromName, getFileIcon
- `frontend/src/utils/format.ts` — formatFileSize, formatDuration, formatTimeOfDay, formatFullTimestamp, formatDayDivider, formatDateTime, …
- `frontend/src/utils/imageProcessing.ts` — ResizeOptions, resizeImage, blurhashFromUrl, captureVideoPoster, validateImageFile, generateThumbnail
- `frontend/src/utils/log.ts` — logIgnored
- `frontend/src/utils/mentions.ts` — mentionsAll
- `frontend/src/utils/notify.ts` — Category, NotifyPayload, loadDeviceCallRingtone, saveDeviceCallRingtone, setNotifyPrefs, notify
- `frontend/src/utils/platform.ts` — isMac, isWindows, isLinux
- `frontend/src/utils/sfx.ts` — playSfx, SFX
- `frontend/src/utils/timeAgo.ts` — timeAgo
- `frontend/src/utils/urlRouting.ts` — deriveSlug, updateURL, parseURL
- `frontend/src/utils/usernameColor.ts` — setBackgroundLightness, isBackgroundLight, useBackgroundIsLight, getUsernameColor
- `frontend/src/utils/voiceWarmup.ts` — warmVoiceChannel
- `frontend/src/hooks/queries/useBlocks.ts` — blocksQueryKeys, useDMRequests, useAcceptDMRequest, useBlockUser, useUnblockUser, useBlockedUsers
- `frontend/src/hooks/queries/useGroups.ts` — groupQueryKeys, useUserGroupsWithChannels, useUserGroups, useCreateGroup, useJoinGroup, …
- `frontend/src/hooks/queries/useMediaPermissions.ts` — PermissionState, MediaPermissions, RevokeResult, MediaKind, useMediaPermissions, openPrivacySettings, …
- `frontend/src/hooks/queries/useMessageRetention.ts` — RETENTION_FOREVER, MESSAGE_RETENTION_OPTIONS, useMessageRetention, useSetMessageRetention
- `frontend/src/hooks/queries/useMessages.ts` — shouldIngest, markIngested, messageQueryKeys, lastMessageQueryKeys, RawChannelMessage, transformChannelMessage, …
- `frontend/src/hooks/queries/usePreferences.ts` — NoiseSuppressionLevel, OverlayMode, normalizeOverlayMode, PreferencesData, SCREEN_SHARE_FPS_OPTIONS, SCREEN_SHARE_FPS_DEFAULT, …
- `frontend/src/hooks/queries/useReactions.ts` — reactionQueryKeys, useReactions, useAddReaction, useRemoveReaction
- `frontend/src/hooks/queries/useSearchMessages.ts` — searchQueryKeys, useSearchMessages
- `frontend/src/hooks/queries/useTransparency.ts` — transparencyQueryKeys, useSelfAuditAccountKey, usePeerAuditAccountKey, useVerifyOwnBuild
- `frontend/src/hooks/queries/useUserProfile.ts` — ServiceUserData, userQueryKeys, useOtherUserProfile, SafetyNumberInfo, PeerVerificationEntry, peerVerificationKeys, …
- `frontend/src/hooks/queries/useVoiceParticipants.ts` — VoiceParticipantInfo, voiceQueryKeys, invalidateVoiceRoom, useVoiceParticipants, useVoiceRoomCounts
- `frontend/src/hooks/useBadge.ts` — useBadge
- `frontend/src/hooks/useLiveKitRealtime.ts` — useLiveKitRealtime
- `frontend/src/hooks/useTauriReady.ts` — useTauriReady, checkIsDesktop
- `frontend/src/hooks/useTypingPublisher.ts` — useTypingPublisher
- `frontend/src/hooks/useTypingState.ts` — useTypingState
- `frontend/src/hooks/useVoiceTest.ts` — VoiceTestPhase, TonePreset, useVoiceTest
- `frontend/src/hooks/useWindowState.ts` — restoreWindowState, useWindowState

---
**Note:** This only covers frontend libraries. The Rust backend lives in `pollis-core/src/commands/`, exposed to the renderer by the Tauri host via the `#[tauri::command]` shims under `src-tauri/src/commands/`; see [commands.md](./commands.md).

_Back to [index.md](./index.md)_
