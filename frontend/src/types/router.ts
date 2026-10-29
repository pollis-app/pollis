// Router context type, in types/ to avoid router.tsx ↔ page import cycles.

export interface RouterContext {
  onLogout: () => void;
  onLock: () => void;
  onDeleteAccount?: () => void;
}
