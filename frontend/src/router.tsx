import {
  createRouter,
  createRoute,
  createRootRouteWithContext,
  createMemoryHistory,
} from "@tanstack/react-router";
import { AppShell } from "./components/Layout/AppShell";
import type { RouterContext } from "./types/router";
import { RootPage } from "./pages/Root";
import { GroupsPage } from "./pages/Groups";
import { GroupPage } from "./pages/Group";
import { ChannelPage } from "./pages/Channel";
import { DMsPage } from "./pages/DMs";
import { DMPage } from "./pages/DM";
import { DMSettingsPage } from "./pages/DMSettings";
import { LeaveGroupPage } from "./pages/LeaveGroup";
import { VoiceChannelPage } from "./pages/VoiceChannel";
import { CreateGroupPage } from "./pages/CreateGroupPage";
import { SearchGroupPage } from "./pages/SearchGroupPage";
import { CreateChannelPage } from "./pages/CreateChannelPage";
import { StartDMPage } from "./pages/StartDMPage";
import { PreferencesPage } from "./pages/PreferencesPage";
import { VoiceSettingsPage } from "./pages/VoiceSettingsPage";
import { SettingsPage } from "./pages/SettingsPage";
import { InvitesPage } from "./pages/InvitesPage";
import { JoinRequestsPage } from "./pages/JoinRequestsPage";
import { AllJoinRequestsPage } from "./pages/AllJoinRequestsPage";
import { InviteMemberPage } from "./pages/InviteMemberPage";
import { MembersPage } from "./pages/MembersPage";
import { KickMemberPage } from "./pages/KickMemberPage";
import { SearchPage } from "./pages/Search";

// Re-export RouterContext so callers can import from either location.
export type { RouterContext };

// ─── Root route ─────────────────────────────────────────────────────────────
// AppShell renders the chrome (TitleBar, VoiceBar, breadcrumb) + <Outlet />
// for the matched child route's content area.

const rootRoute = createRootRouteWithContext<RouterContext>()({
  component: AppShell,
});

// ─── Route definitions ─────────────────────────────────────────────────────────

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: RootPage,
});

const groupsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups",
  component: GroupsPage,
});

// /groups/new and /groups/search must come before /groups/$groupId so they take priority
const createGroupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/new",
  component: CreateGroupPage,
});

const searchGroupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/search",
  component: SearchGroupPage,
});

const groupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/$groupId",
  component: GroupPage,
});

const channelRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/$groupId/channels/$channelId",
  component: ChannelPage,
});

const createChannelRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/$groupId/channels/new",
  component: CreateChannelPage,
});

const membersRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/$groupId/members",
  component: MembersPage,
});

const kickMemberRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/$groupId/members/$userId/kick",
  component: KickMemberPage,
});

const joinRequestsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/$groupId/join-requests",
  component: JoinRequestsPage,
});

const inviteMemberRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/$groupId/invite",
  component: InviteMemberPage,
});

const leaveGroupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/$groupId/leave",
  component: LeaveGroupPage,
});

const voiceChannelRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/groups/$groupId/voice/$channelId",
  component: VoiceChannelPage,
});

const dmsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/dms",
  component: DMsPage,
});

// /dms/new must come before /dms/$conversationId
const startDMRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/dms/new",
  component: StartDMPage,
});

const dmRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/dms/$conversationId",
  component: DMPage,
});

const dmSettingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/dms/$conversationId/settings",
  component: DMSettingsPage,
});

const preferencesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/preferences",
  component: PreferencesPage,
});

const voiceSettingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/voice-settings",
  component: VoiceSettingsPage,
});

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/settings",
  component: SettingsPage,
});

const invitesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/invites",
  component: InvitesPage,
});

const allJoinRequestsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/join-requests",
  component: AllJoinRequestsPage,
});

const searchRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/search",
  component: SearchPage,
});

// ─── Route tree ────────────────────────────────────────────────────────────────

const routeTree = rootRoute.addChildren([
  indexRoute,
  groupsRoute,
  createGroupRoute,
  searchGroupRoute,
  groupRoute,
  channelRoute,
  createChannelRoute,
  membersRoute,
  kickMemberRoute,
  joinRequestsRoute,
  inviteMemberRoute,
  leaveGroupRoute,
  voiceChannelRoute,
  dmsRoute,
  startDMRoute,
  dmRoute,
  dmSettingsRoute,
  preferencesRoute,
  voiceSettingsRoute,
  settingsRoute,
  invitesRoute,
  allJoinRequestsRoute,
  searchRoute,
]);

// ─── Router factory ────────────────────────────────────────────────────────────

export function createAppRouter(context: RouterContext) {
  const memoryHistory = createMemoryHistory({ initialEntries: ["/"] });
  return createRouter({
    routeTree,
    history: memoryHistory,
    context,
  });
}

export type AppRouter = ReturnType<typeof createAppRouter>;

// ─── Router type registration ──────────────────────────────────────────────────
// This enables full type-safety for useParams, useNavigate, etc. throughout
// the app without needing the TanStack Router Vite plugin / codegen.

declare module "@tanstack/react-router" {
  interface Register {
    router: AppRouter;
  }
}
