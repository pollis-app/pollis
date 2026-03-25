// Router context type shared between router.tsx and page components.
// Lives in types/ to avoid circular imports between router.tsx and page files.

export interface RouterContext {
  onLogout: () => void;
  onDeleteAccount?: () => void;
}
