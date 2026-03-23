import { createRootRoute, createRoute, createRouter } from '@tanstack/react-router';
import { Settings } from './pages/Settings';
import { GroupSettings } from './pages/GroupSettings';
import { CreateGroup } from './pages/CreateGroup';
import { CreateChannel } from './pages/CreateChannel';
import { SearchGroup } from './pages/SearchGroup';
import { StartDM } from './pages/StartDM';
import { LeaveDM } from './pages/LeaveDM';
import { MainContent } from './components/Layout/MainContent';
import { RouterLayout } from './components/Layout/RouterLayout';

// Define the router context type
interface RouterContext {
  handleLogout: () => void;
}

// Root route - renders the RouterLayout with Sidebar
const rootRoute = createRootRoute({
  component: () => {
    const { handleLogout } = rootRoute.useRouteContext() as RouterContext;
    return <RouterLayout onLogout={handleLogout} />;
  },
});

// Index route - main content
const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: MainContent,
});

// Settings route
const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings',
  component: Settings,
});

// Create group route
const createGroupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/create-group',
  component: CreateGroup,
});

// Create channel route
const createChannelRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/create-channel',
  component: CreateChannel,
});

// Search group route
const searchGroupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/search-group',
  component: SearchGroup,
});

// Start DM route
const startDMRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/start-dm',
  component: StartDM,
});

// Group routes
const groupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/g/$groupSlug',
  component: MainContent,
});

// Group settings route (sibling of groupRoute, not child)
const groupSettingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/g/$groupSlug/settings',
  component: GroupSettings,
});

// Channel route — sibling of groupRoute with full explicit path so
// MainContent doesn't need to render <Outlet /> for nested routes.
const channelRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/g/$groupSlug/$channelSlug',
  component: MainContent,
});

// DM conversation route
const dmRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/c/$conversationId',
  component: MainContent,
});

// Leave DM confirmation route
const leaveDMRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/c/$conversationId/leave',
  component: LeaveDM,
});

// Create the route tree
const routeTree = rootRoute.addChildren([
  indexRoute,
  settingsRoute,
  createGroupRoute,
  createChannelRoute,
  searchGroupRoute,
  startDMRoute,
  groupSettingsRoute,
  groupRoute,
  channelRoute,
  dmRoute,
  leaveDMRoute,
]);

// Create and export the router
export const router = createRouter({
  routeTree,
  defaultPreload: 'intent',
  context: undefined! as RouterContext, // This will be provided by RouterProvider
});

// Register the router for type safety
declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
