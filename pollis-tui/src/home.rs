//! The pure Home-screen model: flattened sidebar, focus, the open
//! conversation's buffer, scroll/pagination. Async work lives in `app.rs`.

use std::collections::{HashMap, HashSet};

use pollis_core::commands::messages::{ChannelMessage, MessageCursor, Reaction};

use crate::data::ConversationTree;

/// Lines from the top of the loaded buffer at which the next older page is
/// prefetched.
pub const PREFETCH_MARGIN: usize = 5;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Sidebar,
    Messages,
}

/// Which action a bottom-bar prompt feeds; the buffer itself lives on
/// [`crate::app::App::input`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    NewGroup,
    NewChannel { group_id: String, group_name: String },
    /// The buffer is a username or email.
    StartDm,
    /// The buffer is a username or email.
    Invite { group_id: String, group_name: String },
    /// The buffer is an emoji or a `:shortcode:`.
    React { message_id: String },
}

impl PromptKind {
    pub fn label(&self) -> String {
        match self {
            PromptKind::NewGroup => "New group name".to_string(),
            PromptKind::NewChannel { group_name, .. } => {
                format!("New channel in {group_name}")
            }
            PromptKind::StartDm => "Start DM — username or email".to_string(),
            PromptKind::Invite { group_name, .. } => {
                format!("Invite to {group_name} — username or email")
            }
            PromptKind::React { .. } => {
                "React — an emoji or :shortcode: (:+1: :heart: :joy: :tada:)".to_string()
            }
        }
    }
}

/// How the Home screen is consuming keystrokes. The text buffer lives on
/// [`crate::app::App`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum HomeMode {
    #[default]
    Navigate,
    Compose,
    Prompt(PromptKind),
    /// Delete confirmation in the input bar, never a modal.
    ConfirmDelete { message_id: String, preview: String },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum ComposeTarget {
    /// Into the open thread, when one is open.
    #[default]
    New,
    Reply {
        message_id: String,
        sender: String,
        preview: String,
    },
    Edit { message_id: String },
}

#[derive(Debug, Default)]
pub struct ThreadView {
    pub root_id: String,
    /// Oldest first.
    pub replies: Vec<ChannelMessage>,
}

/// Shared by compose and every prompt, so an all-whitespace value never
/// reaches the write layer.
pub fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// Decides whether the pane reads via `data::channel_messages` or
/// `data::dm_messages`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvKind {
    Channel,
    Dm,
    DmRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvRef {
    pub id: String,
    pub name: String,
    pub kind: ConvKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowTarget {
    /// Index into `tree.groups`.
    ToggleGroup(usize),
    Open(ConvRef),
    /// A non-interactive section label.
    Header,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub label: String,
    /// 0 = group/section, 1 = child.
    pub depth: u16,
    pub target: RowTarget,
}

impl SidebarRow {
    pub fn selectable(&self) -> bool {
        !matches!(self.target, RowTarget::Header)
    }
}

/// The open conversation's buffer, kept oldest-first.
#[derive(Debug, Default)]
pub struct OpenConversation {
    pub conv_id: String,
    pub name: String,
    pub kind: Option<ConvKind>,
    pub messages: Vec<ChannelMessage>,
    /// Cursor for the next older page; `None` once exhausted or before the
    /// first load.
    pub older_cursor: Option<MessageCursor>,
    /// Set once a page came back short.
    pub at_beginning: bool,
    /// Lines scrolled up from the bottom (0 = pinned to newest).
    pub scroll: usize,
    pub loading: bool,
    pub reactions: HashMap<String, Vec<Reaction>>,
    /// Thread root id → reply count.
    pub thread_counts: HashMap<String, i64>,
    /// By id, since ids survive a page merge and indices do not. `None` follows
    /// the newest message.
    pub selected: Option<String>,
    pub thread: Option<ThreadView>,
}

impl OpenConversation {
    /// Display order. The channel view hides thread replies (as other clients
    /// do); a thread view is the root followed by its replies.
    pub fn displayed(&self) -> Vec<&ChannelMessage> {
        match &self.thread {
            Some(t) => self
                .messages
                .iter()
                .filter(|m| m.id == t.root_id)
                .chain(t.replies.iter())
                .collect(),
            None => self.messages.iter().filter(|m| m.thread_id.is_none()).collect(),
        }
    }

    pub fn find(&self, id: &str) -> Option<&ChannelMessage> {
        self.messages
            .iter()
            .chain(self.thread.iter().flat_map(|t| t.replies.iter()))
            .find(|m| m.id == id)
    }

    pub fn selected_message(&self) -> Option<&ChannelMessage> {
        self.selected.as_deref().and_then(|id| self.find(id))
    }

    /// `dir` +1 newer, -1 older. Up from nothing selects the newest; down past
    /// the newest clears the selection. Lost-message placeholders are skipped.
    pub fn step_selection(&mut self, dir: i32) {
        let ids: Vec<String> = self
            .displayed()
            .into_iter()
            .filter(|m| !m.undecryptable)
            .map(|m| m.id.clone())
            .collect();
        self.selected = step_message(&ids, self.selected.as_deref(), dir);
    }
}

pub fn step_message(ids: &[String], selected: Option<&str>, dir: i32) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let current = selected.and_then(|s| ids.iter().position(|id| id == s));
    match (current, dir < 0) {
        (None, true) => ids.last().cloned(),
        (None, false) => None,
        (Some(0), true) => ids.first().cloned(),
        (Some(i), true) => ids.get(i - 1).cloned(),
        (Some(i), false) => ids.get(i + 1).cloned(),
    }
}

#[derive(Debug, Default)]
pub struct HomeState {
    pub tree: Option<ConversationTree>,
    /// Expanded group ids.
    pub expanded: HashSet<String>,
    pub rows: Vec<SidebarRow>,
    pub selected: usize,
    pub focus: Focus,
    pub open: Option<OpenConversation>,
    /// Drives the sync spinner.
    pub refreshes: u64,
    pub mode: HomeMode,
    pub compose: ComposeTarget,
}

impl HomeState {
    pub fn new() -> Self {
        Self {
            focus: Focus::Sidebar,
            ..Default::default()
        }
    }

    /// Rebuild the sidebar from a new tree, keeping selection and expansion
    /// where still valid. On the first load every group starts expanded.
    pub fn set_tree(&mut self, tree: ConversationTree, user_id: &str) {
        if self.tree.is_none() {
            for group in &tree.groups {
                self.expanded.insert(group.id.clone());
            }
        }
        self.tree = Some(tree);
        self.rebuild_rows(user_id);
    }

    pub fn rebuild_rows(&mut self, user_id: &str) {
        self.rows = match &self.tree {
            Some(tree) => build_sidebar_rows(tree, &self.expanded, user_id),
            None => Vec::new(),
        };
        self.selected = clamp_selection(&self.rows, self.selected);
    }

    pub fn current_row(&self) -> Option<&SidebarRow> {
        self.rows.get(self.selected)
    }

    pub fn move_selection(&mut self, dir: i32) {
        self.selected = step_selection(&self.rows, self.selected, dir);
    }

    /// Highlight the row that opens `conv_id`, e.g. a just-created
    /// conversation (#1337).
    pub fn select_conversation(&mut self, conv_id: &str) -> Option<ConvRef> {
        let (i, conv) = self.rows.iter().enumerate().find_map(|(i, r)| match &r.target {
            RowTarget::Open(c) if c.id == conv_id => Some((i, c.clone())),
            _ => None,
        })?;
        self.selected = i;
        Some(conv)
    }

    /// The selected row's id iff it is a pending DM request.
    pub fn selected_dm_request(&self) -> Option<String> {
        match &self.current_row()?.target {
            RowTarget::Open(conv) if conv.kind == ConvKind::DmRequest => Some(conv.id.clone()),
            _ => None,
        }
    }

    /// The group that scopes "new channel" / "invite" for the selection.
    pub fn context_group(&self) -> Option<(String, String)> {
        let tree = self.tree.as_ref()?;
        context_group(&self.rows, tree, self.selected)
    }
}

/// The nearest group heading at or above `selected`; `None` once a section
/// header is crossed (DM/request rows have no group).
pub fn context_group(
    rows: &[SidebarRow],
    tree: &ConversationTree,
    selected: usize,
) -> Option<(String, String)> {
    if rows.is_empty() {
        return None;
    }
    let start = selected.min(rows.len() - 1);
    for row in rows[..=start].iter().rev() {
        match &row.target {
            RowTarget::ToggleGroup(i) => {
                let g = tree.groups.get(*i)?;
                return Some((g.id.clone(), g.name.clone()));
            }
            RowTarget::Header => return None,
            RowTarget::Open(_) => {}
        }
    }
    None
}

/// Groups (channels nested when expanded), then Direct Messages, then
/// Requests. `user_id` titles DMs by the other member.
pub fn build_sidebar_rows(
    tree: &ConversationTree,
    expanded: &HashSet<String>,
    user_id: &str,
) -> Vec<SidebarRow> {
    let mut rows = Vec::new();

    for (i, group) in tree.groups.iter().enumerate() {
        let is_open = expanded.contains(&group.id);
        let caret = if is_open { "▾" } else { "▸" };
        rows.push(SidebarRow {
            label: format!("{caret} {}", group.name),
            depth: 0,
            target: RowTarget::ToggleGroup(i),
        });
        if is_open {
            for channel in &group.channels {
                rows.push(SidebarRow {
                    label: format!("# {}", channel.name),
                    depth: 1,
                    target: RowTarget::Open(ConvRef {
                        id: channel.id.clone(),
                        name: channel.name.clone(),
                        kind: ConvKind::Channel,
                    }),
                });
            }
        }
    }

    if !tree.dm_channels.is_empty() {
        rows.push(SidebarRow {
            label: "Direct Messages".to_string(),
            depth: 0,
            target: RowTarget::Header,
        });
        for dm in &tree.dm_channels {
            let name = dm_label(dm, user_id);
            rows.push(SidebarRow {
                label: format!("@ {name}"),
                depth: 1,
                target: RowTarget::Open(ConvRef {
                    id: dm.id.clone(),
                    name,
                    kind: ConvKind::Dm,
                }),
            });
        }
    }

    if !tree.dm_requests.is_empty() {
        rows.push(SidebarRow {
            label: "Requests".to_string(),
            depth: 0,
            target: RowTarget::Header,
        });
        for dm in &tree.dm_requests {
            let name = dm_label(dm, user_id);
            rows.push(SidebarRow {
                label: format!("@ {name} (pending)"),
                depth: 1,
                target: RowTarget::Open(ConvRef {
                    id: dm.id.clone(),
                    name,
                    kind: ConvKind::DmRequest,
                }),
            });
        }
    }

    rows
}

fn dm_label(dm: &pollis_core::commands::dm::DmChannel, user_id: &str) -> String {
    let names: Vec<String> = dm
        .members
        .iter()
        .filter(|m| m.user_id != user_id)
        .map(|m| m.username.clone().unwrap_or_else(|| m.user_id.clone()))
        .collect();
    if names.is_empty() {
        "Direct message".to_string()
    } else {
        names.join(", ")
    }
}

/// The nearest selectable row at or after `selected`, else before it.
pub fn clamp_selection(rows: &[SidebarRow], selected: usize) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let start = selected.min(rows.len() - 1);
    for (i, row) in rows.iter().enumerate().skip(start) {
        if row.selectable() {
            return i;
        }
    }
    for i in (0..start).rev() {
        if rows[i].selectable() {
            return i;
        }
    }
    start
}

/// Step to the next selectable row, staying put at the ends.
pub fn step_selection(rows: &[SidebarRow], from: usize, dir: i32) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let last = rows.len() as i32 - 1;
    let mut i = from as i32;
    loop {
        let next = i + dir;
        if next < 0 || next > last {
            return i.clamp(0, last) as usize;
        }
        i = next;
        if rows[i as usize].selectable() {
            return i as usize;
        }
    }
}

/// A bottom-anchored window: render `[start..end]` after `top_pad` blank
/// lines. `scroll` counts lines up from the bottom, clamped at the top.
pub fn visible_window(total: usize, viewport: usize, scroll: usize) -> (usize, usize, usize) {
    if viewport == 0 {
        return (total, total, 0);
    }
    if total <= viewport {
        return (0, total, viewport - total);
    }
    let max_scroll = total - viewport;
    let s = scroll.min(max_scroll);
    let end = total - s;
    let start = end - viewport;
    (start, end, 0)
}

pub fn should_load_older(scroll: usize, loaded: usize, has_more: bool, loading: bool) -> bool {
    has_more && !loading && scroll + PREFETCH_MARGIN >= loaded
}

/// Merge a fetched page into the oldest-first buffer. The incoming copy wins
/// on a duplicate id, so edits/deletes overwrite; sorted by `(sent_at, id)`.
pub fn merge_messages(
    existing: Vec<ChannelMessage>,
    incoming: Vec<ChannelMessage>,
) -> Vec<ChannelMessage> {
    let mut all = existing;
    let incoming_ids: HashSet<&str> = incoming.iter().map(|m| m.id.as_str()).collect();
    all.retain(|m| !incoming_ids.contains(m.id.as_str()));
    all.extend(incoming);
    all.sort_by(|a, b| (&a.sent_at, &a.id).cmp(&(&b.sent_at, &b.id)));
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use pollis_core::commands::dm::{DmChannel, DmChannelMember};
    use pollis_core::commands::groups::{Channel, GroupWithChannels};

    fn group(id: &str, name: &str, channels: &[(&str, &str)]) -> GroupWithChannels {
        GroupWithChannels {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            owner_id: "owner".to_string(),
            created_at: "t".to_string(),
            current_user_role: "member".to_string(),
            channels: channels
                .iter()
                .map(|(cid, cname)| Channel {
                    id: cid.to_string(),
                    group_id: id.to_string(),
                    name: cname.to_string(),
                    description: None,
                    channel_type: "text".to_string(),
                })
                .collect(),
        }
    }

    fn dm(id: &str, other_user: &str, other_name: Option<&str>) -> DmChannel {
        DmChannel {
            id: id.to_string(),
            created_by: "me".to_string(),
            created_at: "t".to_string(),
            members: vec![
                DmChannelMember {
                    user_id: "me".to_string(),
                    username: Some("me".to_string()),
                    avatar_url: None,
                    added_by: "me".to_string(),
                    added_at: "t".to_string(),
                    accepted_at: Some("t".to_string()),
                },
                DmChannelMember {
                    user_id: other_user.to_string(),
                    username: other_name.map(|s| s.to_string()),
                    avatar_url: None,
                    added_by: "me".to_string(),
                    added_at: "t".to_string(),
                    accepted_at: Some("t".to_string()),
                },
            ],
        }
    }

    fn msg(id: &str, sent_at: &str, content: &str) -> ChannelMessage {
        ChannelMessage {
            id: id.to_string(),
            conversation_id: "c".to_string(),
            sender_id: "s".to_string(),
            sender_username: Some("s".to_string()),
            ciphertext: String::new(),
            content: Some(content.to_string()),
            reply_to_id: None,
            thread_id: None,
            sent_at: sent_at.to_string(),
            edited_at: None,
            deleted_at: None,
            undecryptable: false,
            send_state: None,
            edit_state: None,
        }
    }

    fn tree() -> ConversationTree {
        ConversationTree {
            groups: vec![group("g1", "General", &[("c1", "welcome"), ("c2", "random")])],
            dm_channels: vec![dm("d1", "bob", Some("bob"))],
            dm_requests: vec![dm("r1", "eve", Some("eve"))],
        }
    }

    #[test]
    fn expanded_group_shows_its_channels_dm_and_request_sections() {
        let t = tree();
        let mut expanded = HashSet::new();
        expanded.insert("g1".to_string());
        let rows = build_sidebar_rows(&t, &expanded, "me");
        // group + 2 channels + DM header + 1 dm + Requests header + 1 request
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[0].target, RowTarget::ToggleGroup(0));
        assert!(rows[1].label.contains("welcome"));
        assert_eq!(rows[3].target, RowTarget::Header);
        assert!(rows[4].label.contains("bob"));
        assert!(rows[6].label.contains("eve") && rows[6].label.contains("pending"));
    }

    #[test]
    fn collapsed_group_hides_its_channels() {
        let t = tree();
        let expanded = HashSet::new();
        let rows = build_sidebar_rows(&t, &expanded, "me");
        // group + DM header + 1 dm + Requests header + 1 request (no channels)
        assert_eq!(rows.len(), 5);
        assert!(rows[0].label.starts_with("▸"));
    }

    #[test]
    fn dm_label_prefers_the_other_member() {
        let d = dm("d", "bob", Some("bob"));
        assert_eq!(dm_label(&d, "me"), "bob");
        // A missing username falls back to the id.
        let anon = dm("d", "xyz", None);
        assert_eq!(dm_label(&anon, "me"), "xyz");
    }

    #[test]
    fn step_selection_skips_headers_and_sticks_at_ends() {
        let t = tree();
        let mut expanded = HashSet::new();
        expanded.insert("g1".to_string());
        let rows = build_sidebar_rows(&t, &expanded, "me");
        // From the group (0), down lands on channel row 1, not a header.
        assert_eq!(step_selection(&rows, 0, 1), 1);
        // Stepping down off the DM header (index 3) must skip to the dm (4).
        assert_eq!(step_selection(&rows, 2, 1), 4);
        // At the top, up stays put.
        assert_eq!(step_selection(&rows, 0, -1), 0);
        // At the last selectable row, down stays put.
        let last = rows.len() - 1;
        assert_eq!(step_selection(&rows, last, 1), last);
    }

    #[test]
    fn clamp_selection_never_lands_on_a_header() {
        let rows = vec![
            SidebarRow {
                label: "h".into(),
                depth: 0,
                target: RowTarget::Header,
            },
            SidebarRow {
                label: "x".into(),
                depth: 1,
                target: RowTarget::Open(ConvRef {
                    id: "x".into(),
                    name: "x".into(),
                    kind: ConvKind::Dm,
                }),
            },
        ];
        // Asked for the header (0) → moves forward to the selectable row.
        assert_eq!(clamp_selection(&rows, 0), 1);
        // Out-of-range → clamps into range and onto a selectable row.
        assert_eq!(clamp_selection(&rows, 99), 1);
        // Empty list is a safe 0.
        assert_eq!(clamp_selection(&[], 3), 0);
    }

    #[test]
    fn visible_window_bottom_anchors_and_clamps_scroll() {
        // Fits entirely → top-padded so newest sits at the bottom.
        assert_eq!(visible_window(3, 10, 0), (0, 3, 7));
        // Overflows, pinned to bottom → last 10.
        assert_eq!(visible_window(100, 10, 0), (90, 100, 0));
        // Scrolled up 5 → window shifts back by 5.
        assert_eq!(visible_window(100, 10, 5), (85, 95, 0));
        // Scroll past the top is clamped to the first line.
        assert_eq!(visible_window(100, 10, 999), (0, 10, 0));
        // Zero-height viewport is a no-op.
        assert_eq!(visible_window(5, 0, 0), (5, 5, 0));
    }

    #[test]
    fn should_load_older_fires_only_near_the_top_with_more_history() {
        // Near the top (scroll+margin reaches the loaded count) with a cursor.
        assert!(should_load_older(46, 50, true, false));
        // Comfortably away from the top → no prefetch.
        assert!(!should_load_older(10, 50, true, false));
        // No more history, or already loading → never.
        assert!(!should_load_older(46, 50, false, false));
        assert!(!should_load_older(46, 50, true, true));
    }

    #[test]
    fn is_blank_rejects_empty_and_whitespace_only() {
        assert!(is_blank(""));
        assert!(is_blank("   "));
        assert!(is_blank("\t \n"));
        assert!(!is_blank("hi"));
        assert!(!is_blank("  x  "));
    }

    #[test]
    fn prompt_label_names_the_action_and_its_context() {
        assert_eq!(PromptKind::NewGroup.label(), "New group name");
        assert_eq!(
            PromptKind::NewChannel {
                group_id: "g1".into(),
                group_name: "General".into(),
            }
            .label(),
            "New channel in General"
        );
        assert!(PromptKind::StartDm.label().starts_with("Start DM"));
        assert_eq!(
            PromptKind::Invite {
                group_id: "g1".into(),
                group_name: "General".into(),
            }
            .label(),
            "Invite to General — username or email"
        );
    }

    #[test]
    fn selected_dm_request_only_fires_on_a_pending_request_row() {
        let mut home = HomeState::new();
        home.set_tree(tree(), "me");
        // Row layout (all groups expanded on first load):
        // 0 group, 1 #welcome, 2 #random, 3 "Direct Messages", 4 @bob,
        // 5 "Requests", 6 @eve (pending).
        home.selected = 6;
        assert_eq!(home.selected_dm_request().as_deref(), Some("r1"));
        // A real DM (accepted) is not an acceptable target.
        home.selected = 4;
        assert_eq!(home.selected_dm_request(), None);
        // A channel row is not either.
        home.selected = 1;
        assert_eq!(home.selected_dm_request(), None);
    }

    #[test]
    fn context_group_resolves_from_headings_and_channels_but_not_dms() {
        let mut home = HomeState::new();
        home.set_tree(tree(), "me");
        // Group heading itself → that group.
        home.selected = 0;
        assert_eq!(
            home.context_group(),
            Some(("g1".to_string(), "General".to_string()))
        );
        // A channel row resolves up to its parent group.
        home.selected = 2;
        assert_eq!(
            home.context_group(),
            Some(("g1".to_string(), "General".to_string()))
        );
        // A DM row (past the "Direct Messages" header) has no group context.
        home.selected = 4;
        assert_eq!(home.context_group(), None);
        // A pending request row likewise.
        home.selected = 6;
        assert_eq!(home.context_group(), None);
    }

    #[test]
    fn the_channel_view_hides_thread_replies_and_a_thread_view_shows_them() {
        let mut reply = msg("r1", "2", "in thread");
        reply.thread_id = Some("root".into());
        let mut open = OpenConversation {
            messages: vec![msg("root", "1", "root"), reply, msg("x", "3", "after")],
            ..Default::default()
        };
        let ids = |o: &OpenConversation| -> Vec<String> {
            o.displayed().iter().map(|m| m.id.clone()).collect()
        };
        assert_eq!(ids(&open), vec!["root", "x"]);

        let mut thread_reply = msg("r1", "2", "in thread");
        thread_reply.thread_id = Some("root".into());
        open.thread = Some(ThreadView {
            root_id: "root".into(),
            replies: vec![thread_reply],
        });
        assert_eq!(ids(&open), vec!["root", "r1"]);
    }

    #[test]
    fn message_selection_steps_from_the_newest_and_releases_past_it() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(step_message(&ids, None, -1).as_deref(), Some("c"));
        assert_eq!(step_message(&ids, Some("c"), -1).as_deref(), Some("b"));
        assert_eq!(step_message(&ids, Some("a"), -1).as_deref(), Some("a"));
        assert_eq!(step_message(&ids, Some("b"), 1).as_deref(), Some("c"));
        assert_eq!(step_message(&ids, Some("c"), 1), None);
        assert_eq!(step_message(&ids, None, 1), None);
        // A selection that is no longer loaded restarts from the newest.
        assert_eq!(step_message(&ids, Some("gone"), -1).as_deref(), Some("c"));
        assert_eq!(step_message(&[], None, -1), None);
    }

    #[test]
    fn select_conversation_lands_on_the_new_row() {
        let mut home = HomeState::new();
        home.set_tree(tree(), "me");
        home.selected = 0;
        let conv = home.select_conversation("d1").expect("dm row");
        assert_eq!(conv.kind, ConvKind::Dm);
        assert_eq!(home.selected, 4);
        assert!(home.select_conversation("nope").is_none());
        assert_eq!(home.selected, 4);
    }

    #[test]
    fn merge_messages_dedups_reorders_and_prefers_incoming() {
        let existing = vec![msg("b", "2", "second"), msg("d", "4", "fourth")];
        // An older page (newest-first) plus a re-sent edit of "b".
        let incoming = vec![
            msg("c", "3", "third"),
            msg("a", "1", "first"),
            msg("b", "2", "second-edited"),
        ];
        let merged = merge_messages(existing, incoming);
        let ids: Vec<&str> = merged.iter().map(|m| m.id.as_str()).collect();
        // Sorted oldest-first by (sent_at, id), no dupes.
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
        // The incoming copy of "b" won.
        assert_eq!(merged[1].content.as_deref(), Some("second-edited"));
    }
}
