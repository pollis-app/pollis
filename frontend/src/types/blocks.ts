// Types related to DM requests and user blocking.

export interface BlockedUser {
  user_id: string;
  username: string | null;
  blocked_at: string;
}

// Raw DM channel shape as returned by Tauri commands
// (list_dm_channels, list_dm_requests, create_dm_channel).
export interface DmChannel {
  id: string;
  created_by: string;
  created_at: string;
  members: Array<{
    user_id: string;
    username?: string;
    /** #2085: the display name, with the row. */
    preferred_name?: string | null;
    added_by: string;
    added_at: string;
    accepted_at?: string | null;
  }>;
}
