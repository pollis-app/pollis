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
_Back to [index.md](./index.md)_