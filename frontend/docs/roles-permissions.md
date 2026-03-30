# Roles & Permissions System Design

**Date:** 2026-03-27
**Status:** Proposed
**Scope:** Pollis desktop app (Tauri 2 + Rust + React / Turso libSQL)

---

## 1. Context & Current State

The schema already has the foundations:

- `groups.owner_id` — canonical source of truth for who the owner is.
- `group_member.role TEXT NOT NULL DEFAULT 'member'` — currently stores `'owner'` or `'member'`.
- Several commands in `groups.rs` already check for `'admin'` in the role column (e.g., `remove_member_from_group`, `update_channel`, `delete_channel`) even though `'admin'` is not yet a valid value in the database CHECK constraint. The permission logic is partially forward-compatible but the schema needs to catch up.

Key inconsistency to address: `update_group` and `delete_group` check `groups.owner_id` directly, while channel commands check `group_member.role`. After this change, the convention should be: all permission checks use `group_member.role`, and `groups.owner_id` is reserved for ownership transfer and owner-exclusive actions (delete group, transfer ownership).

---

## 2. Role Model

### 2.1 Roles (Phase 1)

Three built-in roles, ordered by privilege:

| Role     | Stored in           | Description |
|----------|---------------------|-------------|
| `owner`  | `group_member.role` | Group creator. One per group. Has all admin powers plus exclusive owner actions. |
| `admin`  | `group_member.role` | Elevated member. Can manage members and moderate. Multiple per group. |
| `member` | `group_member.role` | Default role. Can read/write messages, send invites. |

The `owner` role in `group_member.role` always mirrors `groups.owner_id`. Both are updated atomically during ownership transfer (already the case in `transfer_ownership`).

### 2.2 Permission Matrix (Phase 1)

| Action                          | owner | admin | member |
|---------------------------------|-------|-------|--------|
| Read messages / channels        | yes   | yes   | yes    |
| Send messages                   | yes   | yes   | yes    |
| Send group invite               | yes   | yes   | yes    |
| Create channel                  | yes   | yes   | no     |
| Update channel                  | yes   | yes   | no     |
| Delete channel                  | yes   | yes   | no     |
| Assign/remove admin role        | yes   | yes   | no     |
| Remove a member                 | yes   | yes   | no     |
| Approve/reject join requests    | yes   | yes   | no     |
| Update group settings           | yes   | yes   | no     |
| Delete group                    | yes   | no    | no     |
| Transfer ownership              | yes   | no    | no     |

> Note: an admin can promote a member to admin or demote another admin to member. An admin cannot affect the `owner` role. Only the owner can reassign ownership via `transfer_ownership`.

---

## 3. Schema Changes

### 3.1 Migration

```sql
-- Migration: add 'admin' to the role CHECK constraint on group_member.
-- libSQL / Turso does not support ALTER TABLE ... MODIFY CONSTRAINT.
-- The constraint must be recreated by rebuilding the table.

-- Step 1: Rename existing table
ALTER TABLE group_member RENAME TO group_member_old;

-- Step 2: Recreate with updated CHECK
CREATE TABLE group_member (
    group_id  TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    user_id   TEXT NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
    role      TEXT NOT NULL DEFAULT 'member'
                   CHECK (role IN ('owner', 'admin', 'member')),
    joined_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (group_id, user_id)
);

-- Step 3: Copy data (all existing rows have role 'owner' or 'member', both valid)
INSERT INTO group_member SELECT * FROM group_member_old;

-- Step 4: Drop old table
DROP TABLE group_member_old;
```

No data migration is needed beyond the table rebuild — no existing rows have `role = 'admin'`.

### 3.2 No Separate `permissions` Table (Phase 1)

A separate `permissions` table is not needed now. The three built-in roles have fixed capabilities expressed entirely in Rust code. Introducing a permissions table before the "custom roles" feature would add complexity with no benefit. See Section 6 for how it fits into the future path.

---

## 4. Rust Command Changes

### 4.1 Internal Helpers

Add shared internal helpers to `groups.rs` to avoid duplicating authorization checks:

```rust
/// Returns the requester's role in a group, or an error if they are not a member.
async fn get_member_role(
    conn: &libsql::Connection,
    group_id: &str,
    user_id: &str,
) -> Result<String>

/// Errors unless the requester holds 'admin' or 'owner' role in the group.
async fn require_admin_or_owner(
    conn: &libsql::Connection,
    group_id: &str,
    requester_id: &str,
) -> Result<()>

/// Errors unless the requester is the owner (checks groups.owner_id).
async fn require_owner(
    conn: &libsql::Connection,
    group_id: &str,
    requester_id: &str,
) -> Result<()>
```

### 4.2 New Command: `set_member_role`

```rust
#[tauri::command]
pub async fn set_member_role(
    group_id: String,
    target_user_id: String,
    new_role: String,       // must be "admin" or "member"
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()>
```

**Authorization rules:**
- `requester` must be `owner` or `admin`.
- `new_role` must be `"admin"` or `"member"`. Passing `"owner"` is an error — use `transfer_ownership`.
- `target_user_id` must be a current member.
- Neither `admin` nor `owner` can change the role of the current `owner`.
- A requester cannot change their own role via this command.

**SQL:**
```sql
UPDATE group_member SET role = ?1
WHERE group_id = ?2 AND user_id = ?3
```

### 4.3 Tighten Existing Commands

| Command | Current check | Required change |
|---------|--------------|-----------------|
| `approve_join_request` | Any member | Require admin or owner |
| `reject_join_request` | Any member | Require admin or owner |
| `update_group` | Owner only (via `owner_id`) | Relax to admin or owner |
| `delete_group` | Owner only | Keep as-is |
| `transfer_ownership` | Owner only | Keep as-is |
| `create_channel` | No check at all | Add admin or owner check |
| `update_channel` | Admin or owner | Already correct |
| `delete_channel` | Admin or owner | Already correct |
| `remove_member_from_group` | Admin or owner | Already correct |

`create_channel` is the most important gap — currently any authenticated user can create a channel.

---

## 5. Frontend Changes

### 5.1 Type Updates (`src/types/index.ts`)

Update `GroupMember` to match what `get_group_members` already returns:

```typescript
export type GroupRole = 'owner' | 'admin' | 'member';

export interface GroupMember {
  user_id: string;
  username?: string;
  display_name?: string;
  avatar_url?: string;
  role: GroupRole;
  joined_at: string;
}

/** Returns true if `role` has at least the privilege level of `minimum`. */
export function hasAtLeastRole(role: GroupRole, minimum: GroupRole): boolean {
  const order: GroupRole[] = ['member', 'admin', 'owner'];
  return order.indexOf(role) >= order.indexOf(minimum);
}
```

### 5.2 New Hooks (`useGroups.ts`)

Add `members` to `groupQueryKeys`:
```typescript
members: (groupId: string) => ["groups", groupId, "members"] as const,
```

**`useGroupMembers(groupId: string | null)`** — fetches all members via existing `get_group_members` command.

**`useSetMemberRole()`** — mutation for the new `set_member_role` command; on success, invalidates `groupQueryKeys.members(groupId)`.

**`useCurrentMemberRole(groupId: string | null)`** — derived: wraps `useGroupMembers`, returns the current user's `GroupRole | null` for a given group. Used throughout the UI to gate controls.

### 5.3 New Page: Member Management

**Route:** `/groups/$groupId/members`

**Files:** `src/pages/GroupMembersPage.tsx` (shell) + `src/pages/GroupMembers.tsx` (content)

Shows all members with role badges. For viewers with `admin` or `owner` role:
- Role dropdown next to each non-owner member (`admin` / `member`)
- Remove button next to each non-owner member
- Owner row is visually distinguished and cannot be acted on

Accessible from `GroupSettings.tsx` via a "Manage Members" link gated to `admin` or `owner`.

### 5.4 Existing Page Updates

**`JoinRequests.tsx`** — approve/reject buttons should be hidden for plain `member` viewers. The Rust commands enforce this server-side; the frontend guard is UX only.

**`GroupSettings.tsx`** — "Delete Group" action stays owner-only. The "Manage Members" navigation link should only render for `admin` or `owner`.

---

## 6. Future Extensibility: Custom Roles & Per-Channel Permissions

### 6.1 Phase 2 Schema

When custom roles are required, add without breaking Phase 1:

```sql
CREATE TABLE group_role (
    id         TEXT PRIMARY KEY,
    group_id   TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    color      TEXT,
    position   INTEGER NOT NULL DEFAULT 0,
    is_builtin INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (group_id, name)
);

CREATE TABLE group_role_permission (
    role_id    TEXT NOT NULL REFERENCES group_role(id) ON DELETE CASCADE,
    permission TEXT NOT NULL,           -- e.g. 'manage_members', 'manage_channels'
    PRIMARY KEY (role_id, permission)
);

-- group_member gets a nullable FK; existing rows keep the legacy `role` string
ALTER TABLE group_member ADD COLUMN role_id TEXT REFERENCES group_role(id);
```

Resolution: check `role_id` first (custom role permissions table), fall back to the legacy `role` string for built-in roles.

### 6.2 Per-Channel Permission Overrides

```sql
CREATE TABLE channel_role_override (
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    role_id    TEXT NOT NULL REFERENCES group_role(id) ON DELETE CASCADE,
    permission TEXT NOT NULL,
    granted    INTEGER NOT NULL DEFAULT 1,  -- 1 = grant, 0 = deny
    PRIMARY KEY (channel_id, role_id, permission)
);
```

Resolution order (highest wins): explicit channel deny > explicit channel grant > group role permission > built-in default.

---

## 7. Implementation Sequence

1. **Schema migration** — rebuild `group_member` with `'admin'` in CHECK. Deploy to Turso.
2. **Rust helpers** — add `get_member_role`, `require_admin_or_owner`, `require_owner`.
3. **Tighten existing commands** — `approve_join_request`, `reject_join_request`, `update_group`, `create_channel`.
4. **New command** — `set_member_role`, registered in `lib.rs`.
5. **Frontend types** — update `GroupMember`, add `GroupRole` and `hasAtLeastRole`.
6. **Frontend hooks** — `useGroupMembers`, `useSetMemberRole`, `useCurrentMemberRole`.
7. **Members page** — `GroupMembersPage.tsx` + `GroupMembers.tsx`, route in `router.tsx`.
8. **Access guards** — `GroupSettings.tsx` and `JoinRequests.tsx`.

---

## 8. Open Questions

- **Admin cap**: Maximum number of admins per group? Not required for Phase 1 but affects UI.
- **Audit log**: Should role changes be recorded in a `group_audit_log` table for moderation?
- **Role change notification**: Should the affected member receive an in-app notification?
