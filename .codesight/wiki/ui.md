# UI

> **Navigation aid.** Component inventory and prop signatures extracted via AST. Read the source files before adding props or modifying component logic.

**66 components** (react)

## Components

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
- **WindowResizeEdges** — Linux-only invisible resize handles for the frameless window — `frontend/src/components/Layout/WindowResizeEdges.tsx`
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
- **KeyboardShortcutsPage** — "Key Bindings" global keyboard-shortcut reference (route `/shortcuts`, linked from the Account hub) — `frontend/src/pages/KeyboardShortcutsPage.tsx`
- **KickMemberPage** — `frontend/src/pages/KickMemberPage.tsx`
- **LeaveGroupPage** — `frontend/src/pages/LeaveGroup.tsx`
- **Members** — props: groupId, isAdmin — `frontend/src/pages/Members.tsx`
- **MembersPage** — `frontend/src/pages/MembersPage.tsx`
- **Preferences** — includes the "Local message history" retention control (Forever / 1 year / 90 / 30 days), which is **device-local** — stored in the local `ui_state` table, not synced across the user's devices (see [database.md](./database.md#local-message-retention)) — `frontend/src/pages/Preferences.tsx`
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

## Window chrome (frameless)

The window ships `decorations: false` (`src-tauri/tauri.conf.json`). macOS
re-adds `NSResizableWindowMask` and Windows uses the DWM frame (`src-tauri/src/lib.rs`),
so both get native title-bar drag + a comfortable resize border. The custom
**TitleBar** provides the drag region (`startDragging`) and window controls.

Linux/Wayland gives an undecorated toplevel **no** server-side resize edge, so
**WindowResizeEdges** overlays eight invisible strips (four edges + four
corners) that call `startResizeDragging(direction)` via the window bridge —
widening the grab area from the ~1px compositor edge to a comfortable 6px
edge / 12px corner. Gated to Tauri + Linux (`navigator.userAgent` match);
elsewhere the native frame already handles resize. Needs the
`core:window:allow-start-resize-dragging` capability
(`src-tauri/capabilities/default.json`).

## Theming & skins

All colors route through `--c-*` CSS custom properties defined in `frontend/src/index.css` and surfaced as semantic Tailwind utilities (`bg-bg`, `bg-surface`, `text-fg`, `border-line`, …) in `frontend/tailwind.config.js`. The palette is derived at runtime from six "knob" vars — `--accent-h/s/l` and `--bg-h/s/l` — plus `--font-size-base` (all `rem` sizes scale off it) and `--bar-h`. `applyAccentColor` / `applyBackgroundColor` / `applyFontSize` in `frontend/src/utils/colorUtils.ts` write the knobs; `applyPreferences` (`hooks/queries/usePreferences.ts`) drives them from the synced preferences blob. Corner radii are tokenized as `--radius-chip` / `--radius-control`.

### UI skins (issue #565)

Two skins share the same `--c-*` token names:

- **`terminal`** (default) — the IRC/monospace look; base `:root`.
- **`refined`** — a friendlier, proportional-sans, Slack/Discord-shaped alternate for users who dislike the terminal aesthetic. A `:root[data-skin="refined"]` block **overrides** the same tokens (warm-charcoal surfaces, neutral near-white text so the amber wash lifts, neutral borders, softer radii, and `--font-sans` → Geist Sans, self-hosted via `@fontsource/geist-sans`; terminal keeps Atkinson Hyperlegible) and inverts the monospace default: `.font-mono:not(.font-machine)` renders as sans. Machine-facing text (timestamps, kbd chips, `#slug`s, code, metrics) opts back into mono with the `.font-machine` class — stays mono in **both** skins.

The skin is a synced preference (`PreferencesData.skin`, rides in the opaque preferences JSON blob — no migration/Rust change), applied via `applySkin(skin)` → `document.documentElement.dataset.skin`. Toggle lives in the **Appearance** section of `PreferencesPage`. Because every surface already routes color through `--c-*`, the skin is an overlay that reskins the whole app (including legacy inline `var(--c-…)` call sites) without a parallel component tree.

For surfaces whose *structure* (not just color) differs between skins, components branch on `useSkin()` (reactive, from `usePreferences.ts`) and keep the terminal render path unchanged. Refined structural forks: the message row (`MessageItem`/`MessageList` — Slack-style avatar gutter, name+timestamp header with body below, consecutive-sender grouping, centered date dividers; attachment rendering extracted to `Message/AttachmentDisplay.tsx`), the sidebar bottom (`SidebarProfilePanel` — Discord-style identity + a persistent voice strip that replaces the standalone `VoiceBar`, which AppShell renders in terminal only), the sidebar DM rows (`Sidebar` — a `PresenceAvatar` for the peer, presence dot anchored to it, where terminal shows a bare `PresenceDot`), the breadcrumb (`BreadcrumbNav`), the voice stage metrics, and the bottom status bar (neutral fill instead of terminal's accent fill).

### Refined layout spacing

The refined skin uses a roomier rhythm than terminal via CSS tokens (rem, so they track the font-size preference): `--side-w` (sidebar width, `w-[var(--side-w)]`), `--lh` (message-body line-height), and message spacing (`--msg-header-gap` before a sender group, `--msg-group-gap` between grouped messages, `--msg-row-pad-y` per row, `--msg-divider-gap` around date dividers). These are set in the `:root[data-skin="refined"]` block; terminal keeps the base `:root` values. (An earlier comfortable/compact density toggle was removed — the delta wasn't worth a user control.)

---
_Back to [index.md](./index.md)_