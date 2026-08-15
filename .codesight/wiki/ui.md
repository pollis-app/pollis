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

### Presence

`presenceStore` infers online-ness from LiveKit room participation, keeping a
`user_id → Set<room_id>` map so leaving one shared room doesn't flip someone
offline while they're still in another. Two properties are load-bearing:

- **`isOnline` is deliberately NOT a MobX action.** Actions run untracked, so an
  `action` here would leave its `byUser` reads invisible to the calling
  `observer()` and presence would stop updating live. It stays a plain method,
  excluded via `makeAutoObservable(this, { isOnline: false })`.
- **The local user is tracked separately.** Room events only ever describe
  *other* participants — nothing tells us we are online — so `byUser` never
  contains us and every surface would report the signed-in user as offline.
  `appStore.setCurrentUser` (and `logout`) pushes the id into
  `presenceStore.setSelfUserId`, and `isOnline` short-circuits true for it. The
  dependency is one-way (`appStore → presenceStore`; presence imports nothing of
  ours) so it cannot cycle. Don't paper over this at a call site with a
  hardcoded `presence="online"`.

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

### `components/Layout` (14)

- **AppShell** — `frontend/src/components/Layout/AppShell.tsx`
- **BreadcrumbNav** — `frontend/src/components/Layout/BreadcrumbNav.tsx`
- **MainContent** — props: pendingDmRequest — `frontend/src/components/Layout/MainContent.tsx`
- **PageShell** — props: title, children, scrollable — `frontend/src/components/Layout/PageShell.tsx`
- **Sidebar** — props: isOpen, onToggle — `frontend/src/components/Layout/Sidebar.tsx`
- **SidebarProfilePanel** — refined only; identity row + the persistent voice strip (channel, mic, deafen, screenshare, disconnect) that replaces terminal's `VoiceBar` — `frontend/src/components/Layout/SidebarProfilePanel.tsx`
- **StatusBarSummary** — props: icon, count, to, label, color, testId — `frontend/src/components/Layout/StatusBarSummary.tsx`
- **TitleBar** — `frontend/src/components/Layout/TitleBar.tsx`
- **WindowResizeEdges** — `frontend/src/components/Layout/WindowResizeEdges.tsx`

#### `components/Layout/RightPanel` — right-hand context panel (#824)

- **RightPanel** — the generic slot — `frontend/src/components/Layout/RightPanel/RightPanel.tsx`
- **MembersPanel** — props: groupId, channelId, conversationId — `frontend/src/components/Layout/RightPanel/MembersPanel.tsx`
- **ThreadPanel** — props: threadId, channelId, conversationId — `frontend/src/components/Layout/RightPanel/ThreadPanel.tsx`
- **ThreadMessageRow** — props: message, isRoot — `frontend/src/components/Layout/RightPanel/ThreadMessageRow.tsx`
- **MemberRow** — props: userId, label, avatarKey, isAdmin — `frontend/src/components/Layout/RightPanel/MemberRow.tsx`
- **MediaGrid** — props: attachments — `frontend/src/components/Layout/RightPanel/MediaGrid.tsx`
- **MediaTile** — props: attachment — `frontend/src/components/Layout/RightPanel/MediaTile.tsx`

The panel is a **flex sibling of `<Outlet />`** in `AppShell`, never an overlay — opening it reflows the message list. Which panel is open lives in the URL, not in a store:

| `?panel=` | Meaning |
| --- | --- |
| absent | Follow the default: the `right_panel_open_by_default` preference, else the skin (open in `refined`, closed in `terminal`). |
| `members` | Explicitly open. |
| `thread` | A thread is open; `?thread=<ULID>` names its root. Required iff `panel=thread` — `validatePanelSearch` drops the whole panel param rather than admit one without the other, so "thread open with no thread" is unrepresentable. |
| `none` | Explicitly closed — distinct from absent, so a `refined` user can share a link with the panel shut. |

`validatePanelSearch` (`frontend/src/types/panel.ts`) is declared on the **root** route, because the panel is AppShell's chrome rather than any one page's. It drops unrecognised values instead of throwing, so a stale or hand-edited link falls back to the default rather than blanking the app. `useRightPanel` resolves the precedence and owns the only write path.

`panel` is a discriminated union, not a boolean, so threads (#825) add a variant rather than a second sidebar. **`useRightPanel` navigates with an explicit `to: pathname`** — a relative `to: "."` would resolve against `/` (AppShell renders at the root route) and throw the user out of their conversation, and omitting `to` leaves the search type unresolved.

**Chrome mirrors the left sidebar.** The panel's close affordance is a **footer**, not a header: a full-width `min-h-bar border-t border-line` button with the label on the left and a `<kbd>` on the right carrying the live `useShortcutLabel("app.toggleRightPanel")` combo (default `mod+shift+b`, the shifted twin of the sidebar's `mod+b`). AppShell binds the same command id, so the chip and the key can't drift. Width tracks `--side-w` and the aside carries `font-mono`, which is the entire skin switch — terminal renders DM Mono, refined re-points `.font-mono:not(.font-machine)` at the sans face. Terminal additionally borrows the sidebar's `SectionHeader` chrome (sticky `h-bar` strip, hairline under, a second hairline above every section after the first) and its one-text-line rows with a bare `PresenceDot`; refined keeps the airier avatar rows.

**Content is contextual, the column is not.** The panel no longer collapses on routes with no conversation (Preferences, Search, root). `MembersPanel` degrades there to the plain online roster — self plus everyone visible in `presenceStore.byUser`, named from the DM list — and drops the media grid, since "who is online" still answers a question off-conversation and "media shared in this conversation" does not. A stale `?panel=thread` on such a route falls back to the roster rather than rendering an empty thread.

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

### `components/Voice` (9)

- **CameraPicker** — `frontend/src/components/Voice/CameraPicker.tsx`
- **SidebarVoiceControls** — mic (four-state) + deafen for refined's sidebar voice strip; shares `micIndicatorOf` and `voiceControlLabels` with `VoiceBar` / `VoiceStage` — `frontend/src/components/Voice/SidebarVoiceControls.tsx`
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
- **PresenceDot** — props: userId, testId — bare online/offline dot for rows with no avatar to anchor it (terminal sidebar DMs, right-panel members) — `frontend/src/components/ui/PresenceDot.tsx`
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
