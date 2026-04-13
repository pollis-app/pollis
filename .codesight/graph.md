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
