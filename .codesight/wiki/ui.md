# UI

> **Navigation aid, regenerated — not hand-maintained.** Component inventory and prop
> signatures extracted from the source. Read the source files before adding props or
> modifying component logic.
>
> This inventory had drifted badly (it listed 14 components that no longer existed and
> omitted roughly 50 that did — #804), so treat the filesystem as the authority and
> **regenerate rather than patch** when it disagrees. Prop lists come from each file's
> `*Props` type and are omitted where a component takes none or builds its props inline.

**111 `.tsx` files** under `frontend/src`

## Conventions

- Reusable components live in **their own files**, never inlined in a parent.
- Use the shared primitives in `frontend/src/components/ui/` (`Button`, `Switch`,
  `TextInput`, …) — never a hand-rolled styled equivalent.
- **No modals.** Confirmation and input flows replace the chat input bar (the
  edit/delete bar pattern in `MainContent`) or navigate to a page. The sole exception
  is the Cmd+K search menu.
- Components read MobX stores inside `observer()` wrappers; remote data comes from
  React Query hooks in `frontend/src/hooks/queries/`.

## Components

### `(root)` (3)

- **App** — `frontend/src/App.tsx`
- **main** — `frontend/src/main.tsx`
- **router** — `frontend/src/router.tsx`

### `components` (6)

- **ErrorBoundary** — `frontend/src/components/ErrorBoundary.tsx`
- **SearchPanel** — props: isOpen, onClose — `frontend/src/components/SearchPanel.tsx`
- **TerminalApp** — props: onLogout, onLock, onDeleteAccount — `frontend/src/components/TerminalApp.tsx`
- **TerminalView** — props: visible — `frontend/src/components/TerminalView.tsx`
- **TypingIndicator** — props: channelId, conversationId — `frontend/src/components/TypingIndicator.tsx`
- **UpdateScreen** — `frontend/src/components/UpdateScreen.tsx`

### `components/Auth` (7)

- **EmailOTPAuth** — props: onSuccess, prefillEmail, prefillNonce, onStepChange — `frontend/src/components/Auth/EmailOTPAuth.tsx`
- **EnrollmentApprovalPrompt** — props: requestId, newDeviceId, verificationCode, onResolved — `frontend/src/components/Auth/EnrollmentApprovalPrompt.tsx`
- **EnrollmentGateScreen** — props: userId, userEmail, onEnrolled, onCancel, onResetComplete — `frontend/src/components/Auth/EnrollmentGateScreen.tsx`
- **LoginScreen** — props: knownAccounts, onAuthSuccess, onWipeComplete — `frontend/src/components/Auth/LoginScreen.tsx`
- **PinCreateScreen** — props: oldPin, onCreated, onCancel, headline, subline — `frontend/src/components/Auth/PinCreateScreen.tsx`
- **PinEntryScreen** — props: userId, username, onUnlocked, onForgotPin, onSwitchAccount — `frontend/src/components/Auth/PinEntryScreen.tsx`
- **SaveSecretKeyScreen** — props: secretKey, email, onConfirmed — `frontend/src/components/Auth/SaveSecretKeyScreen.tsx`

### `components/Layout` (9)

- **AppShell** — `frontend/src/components/Layout/AppShell.tsx`
- **BreadcrumbNav** — `frontend/src/components/Layout/BreadcrumbNav.tsx`
- **MainContent** — props: pendingDmRequest — `frontend/src/components/Layout/MainContent.tsx`
- **PageShell** — props: title, children, scrollable — `frontend/src/components/Layout/PageShell.tsx`
- **Sidebar** — props: isOpen, onToggle — `frontend/src/components/Layout/Sidebar.tsx`
- **SidebarProfilePanel** — `frontend/src/components/Layout/SidebarProfilePanel.tsx`
- **StatusBarSummary** — props: icon, count, to, label, color, testId — `frontend/src/components/Layout/StatusBarSummary.tsx`
- **TitleBar** — `frontend/src/components/Layout/TitleBar.tsx`
- **WindowResizeEdges** — `frontend/src/components/Layout/WindowResizeEdges.tsx`

### `components/Message` (9)

- **AttachmentDisplay** — `frontend/src/components/Message/AttachmentDisplay.tsx`
- **LastMessagePreview** — props: channelId, conversationId — `frontend/src/components/Message/LastMessagePreview.tsx`
- **MediaLinkUnfurl** — props: text — `frontend/src/components/Message/MediaLinkUnfurl.tsx`
- **MessageAvatar** — props: userId, username, size — `frontend/src/components/Message/MessageAvatar.tsx`
- **MessageItem** — props: message, allMessages, authorUsername, isAuthorAdmin, canModerate, isGroupStart, onReply, onEdit, onDelete, onPin, onScrollToReply — `frontend/src/components/Message/MessageItem.tsx`
- **MessageList** — props: messages, conversationId, groupIdForNames, adminUserIds, viewerIsAdmin, onReply, onEdit, onDelete, onPin, onScrollToMessage, getAuthorUsername, hasMore, isFetchingMore, onLoadMore — `frontend/src/components/Message/MessageList.tsx`
- **MessageQueue** — `frontend/src/components/Message/MessageQueue.tsx`
- **MessageReactions** — props: messageId — `frontend/src/components/Message/MessageReactions.tsx`
- **ReplyPreview** — props: messageId, allMessages, onDismiss, onScrollToMessage — `frontend/src/components/Message/ReplyPreview.tsx`

### `components/Search` (1)

- **SearchView** — props: onNavigateToConversation — `frontend/src/components/Search/SearchView.tsx`

### `components/Security` (3)

- **AccountKeyAuditLine** — props: status, detail, testId — `frontend/src/components/Security/AccountKeyAuditLine.tsx`
- **BuildVerifyLine** — props: status, detail, testId — `frontend/src/components/Security/BuildVerifyLine.tsx`
- **KeyChangeBanner** — props: peerUserId, peerLabel — `frontend/src/components/Security/KeyChangeBanner.tsx`

### `components/Voice` (8)

- **CameraPicker** — `frontend/src/components/Voice/CameraPicker.tsx`
- **RemoteUserVolumeSlider** — props: identity, participantName — `frontend/src/components/Voice/RemoteUserVolumeSlider.tsx`
- **RemoteVideoTile** — props: trackKey, className, initialWidth, initialHeight, preview, mirror — `frontend/src/components/Voice/RemoteVideoTile.tsx`
- **ScreenSharePicker** — props: disabled, onPick, title, subtitle, thumbnail, icon — `frontend/src/components/Voice/ScreenSharePicker.tsx`
- **ScreenShareViewer** — `frontend/src/components/Voice/ScreenShareViewer.tsx`
- **VoiceBar** — props: channelId, channelName — `frontend/src/components/Voice/VoiceBar.tsx`
- **StageTile** — props: participant, mode, focused, onFocus, onView — `frontend/src/components/Voice/stage/StageTile.tsx`
- **VoiceStage** — props: channelName, isInCall, onJoin, onLeave, onBack, onOpenSettings, observerParticipants, headerActions, footer, callMode — `frontend/src/components/Voice/stage/VoiceStage.tsx`

### `components/ui` (22)

- **AudioPlayer** — props: src, title, className, autoPlay, loop, preload — `frontend/src/components/ui/AudioPlayer.tsx`
- **Avatar** — props: avatarKey, size, alt, testId, variant, presence — `frontend/src/components/ui/Avatar.tsx`
- **Button** — props: children, onClick, disabled, isLoading, loadingText, className, variant, size, type, onKeyDown, autoFocus — `frontend/src/components/ui/Button.tsx`
- **Card** — props: children, className, style, padding — `frontend/src/components/ui/Card.tsx`
- **ChatInput** — props: onSend, placeholder, disabled, autoFocus, className, maxAttachments, onValueChange, draftKey, canNotifyAll — `frontend/src/components/ui/ChatInput.tsx`
- **Checkbox** — props: label, checked, onChange, disabled, className — `frontend/src/components/ui/Checkbox.tsx`
- **ALL_ALGORITHMS** — props: algorithm, dotSize, spacing, speed, className, style — `frontend/src/components/ui/DotMatrix.tsx`
- **InlineAudioPlayer** — props: src, title, className, autoPlay, onClick — `frontend/src/components/ui/InlineAudioPlayer.tsx`
- **InputOtp** — props: length, value, onChange, disabled, autoFocus, mask — `frontend/src/components/ui/InputOtp.tsx`
- **LinkifiedText** — props: text — `frontend/src/components/ui/LinkifiedText.tsx`
- **LoadingSpinner** — props: size, className — `frontend/src/components/ui/LoaderSpinner.tsx`
- **NavigableGrid** — `frontend/src/components/ui/NavigableGrid.tsx`
- **NavigableList** — `frontend/src/components/ui/NavigableList.tsx`
- **PillButton** — props: accent, onClick, title, square, children — `frontend/src/components/ui/PillButton.tsx`
- **PollisLogo** — props: size, color — `frontend/src/components/ui/PollisLogo.tsx`
- **PresenceAvatar** — props: userId, avatarKey, size, alt, testId, variant — `frontend/src/components/ui/PresenceAvatar.tsx`
- **RangeSlider** — props: label, value, onChange, min, max, step, disabled, className, id, sublabel, description — `frontend/src/components/ui/RangeSlider.tsx`
- **ScrambleText** — props: text, placeholderLength, typeSpeed, scrambleInterval, className — `frontend/src/components/ui/ScrambleText.tsx`
- **Switch** — props: label, checked, onChange, disabled, className, id, description — `frontend/src/components/ui/Switch.tsx`
- **TerminalMenu** — props: items, onEsc, className, autoFocus — `frontend/src/components/ui/TerminalMenu.tsx`
- **TextArea** — props: label, value, onChange, placeholder, description, error, disabled, rows, className, id — `frontend/src/components/ui/TextArea.tsx`
- **TextInput** — props: label, value, onChange, placeholder, description, error, disabled, type, className, id, required, autoFocus, autoComplete — `frontend/src/components/ui/TextInput.tsx`

### `pages` (43)

- **AllJoinRequestsPage** — `frontend/src/pages/AllJoinRequestsPage.tsx`
- **ArcadePage** — `frontend/src/pages/ArcadePage.tsx`
- **BlockedPage** — `frontend/src/pages/BlockedPage.tsx`
- **CallPage** — `frontend/src/pages/Call.tsx`
- **ChangePinPage** — `frontend/src/pages/ChangePinPage.tsx`
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
- **InvitesPage** — `frontend/src/pages/InvitesPage.tsx`
- **JoinRequests** — props: groupId, groupName — `frontend/src/pages/JoinRequests.tsx`
- **JoinRequestsPage** — `frontend/src/pages/JoinRequestsPage.tsx`
- **KeyboardShortcutsPage** — `frontend/src/pages/KeyboardShortcutsPage.tsx`
- **KickMemberPage** — `frontend/src/pages/KickMemberPage.tsx`
- **LeaveGroupPage** — `frontend/src/pages/LeaveGroup.tsx`
- **Members** — props: groupId, isAdmin — `frontend/src/pages/Members.tsx`
- **MembersPage** — `frontend/src/pages/MembersPage.tsx`
- **PreferencesPage** — `frontend/src/pages/PreferencesPage.tsx`
- **RenameChannel** — props: groupId, channelId, onSuccess — `frontend/src/pages/RenameChannel.tsx`
- **RenameChannelPage** — `frontend/src/pages/RenameChannelPage.tsx`
- **RenameGroup** — props: groupId, onSuccess — `frontend/src/pages/RenameGroup.tsx`
- **RenameGroupPage** — `frontend/src/pages/RenameGroupPage.tsx`
- **RequestsPage** — `frontend/src/pages/RequestsPage.tsx`
- **RootPage** — `frontend/src/pages/Root.tsx`
- **SearchPage** — `frontend/src/pages/Search.tsx`
- **SearchGroupPage** — `frontend/src/pages/SearchGroupPage.tsx`
- **SecurityPage** — `frontend/src/pages/SecurityPage.tsx`
- **SettingsHubPage** — `frontend/src/pages/SettingsHub.tsx`
- **SettingsPage** — `frontend/src/pages/SettingsPage.tsx`
- **StartDM** — props: onSuccess — `frontend/src/pages/StartDM.tsx`
- **StartDMPage** — `frontend/src/pages/StartDMPage.tsx`
- **UpdatePage** — `frontend/src/pages/UpdatePage.tsx`
- **UserProfilePage** — `frontend/src/pages/UserProfile.tsx`
- **VoiceChannelPage** — `frontend/src/pages/VoiceChannel.tsx`
- **VoiceSettingsPage** — props: label, devices, value, onChange, fallbackLabel — `frontend/src/pages/VoiceSettingsPage.tsx`

---

_Back to [index.md](./index.md)_
