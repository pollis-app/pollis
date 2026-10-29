//! The M2b three-pane client state — the interactive shell on top of the
//! already-gated read/sync layer (`pollis_tui::{data, sync}`).
//!
//! The pure, in-memory Home screen model; async loading lives in `app.rs`.

use std::collections::{HashMap, HashSet};

use pollis_core::commands::messages::{ChannelMessage, MessageCursor, Reaction};

use crate::data::ConversationTree;

/// How close to the top of the loaded buffer the user must scroll before we
/// prefetch the next older page.
pub const PREFETCH_MARGIN: usize = 5;

/// Which pane currently receives navigation keys.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Sidebar,
    Messages,
}

/// What a bottom-bar text prompt (a create/invite flow) is collecting. The
/// active buffer lives on [`crate::app::App::input`]; this records the action and its context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    /// New group; the buffer is the group name.
    NewGroup,
    /// New channel in an existing group; the buffer is the channel name.
    NewChannel { group_id: String, group_name: String },
    /// Start a DM; the buffer is the other user's username/email.
    StartDm,
    /// Invite a user to a group; the buffer is the invitee's username/email.
    Invite { group_id: String, group_name: String },
    /// React to a message; the buffer is an emoji or a `:shortcode:`.
    React { message_id: String },
}

impl PromptKind {
    /// The label shown on the bottom input bar, naming what's being collected.
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

/// How the Home screen consumes keystrokes; buffers live on [`crate::app::App`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum HomeMode {
    /// Navigation + command keys (the default).
    #[default]
    Navigate,
    /// Typing a message into the open conversation.
    Compose,
    /// Filling a create/invite prompt (bottom input bar).
    Prompt(PromptKind),
    /// Confirming deletion of the user's own message in the input bar (never a modal).
    ConfirmDelete { message_id: String, preview: String },
}

/// What the compose bar's Enter does with the buffer.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum ComposeTarget {
    /// A new message (into the open thread, when one is open).
    #[default]
    New,
    /// A quote reply to `message_id` (`reply_to_id`).
    Reply {
        message_id: String,
        sender: String,
        preview: String,
    },
    /// Replace the body of the user's own `message_id`.
    Edit { message_id: String },
}

/// The open thread, when the message pane shows one instead of the channel.
#[derive(Debug, Default)]
pub struct ThreadView {
    pub root_id: String,
    /// Replies, oldest first (`read_thread_messages`).
    pub replies: Vec<ChannelMessage>,
}

/// Non-empty after trimming, so all-whitespace never reaches the write layer.
pub fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// Which read path the message pane uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvKind {
    Channel,
    Dm,
    DmRequest,
}

/// A concrete, openable conversation: enough to fetch and title it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvRef {
    pub id: String,
    pub name: String,
    pub kind: ConvKind,
}

/// What activating a sidebar row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowTarget {
    /// A group heading — Enter toggles its expansion. Carries the group index.
    ToggleGroup(usize),
    /// A leaf conversation — Enter opens it.
    Open(ConvRef),
    /// A non-interactive section label ("Direct Messages", "Requests").
    Header,
}

/// One rendered line of the left pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub label: String,
    /// Indentation level (0 = group/section, 1 = child).
    pub depth: u16,
    pub target: RowTarget,
}

impl SidebarRow {
    /// Whether navigation is allowed to land on this row (headers are skipped).
    pub fn selectable(&self) -> bool {
        !matches!(self.target, RowTarget::Header)
    }
}

/// The open conversation's message buffer plus its scroll/pagination state.
/// Messages are kept **oldest-first** (display order: newest at the bottom).
#[derive(Debug, Default)]
pub struct OpenConversation {
    pub conv_id: String,
    pub name: String,
    pub kind: Option<ConvKind>,
    /// Loaded messages, oldest-first.
    pub messages: Vec<ChannelMessage>,
    /// Cursor for the next older page; `None` once exhausted or before the first load.
    pub older_cursor: Option<MessageCursor>,
    /// True once a page came back short — the beginning of history is loaded.
    pub at_beginning: bool,
    /// Lines scrolled up from the bottom (0 = pinned to newest).
    pub scroll: usize,
    /// True while the first page is still loading (drives the empty state).
    pub loading: bool,
    /// Reactions per message id, for the messages on screen.
    pub reactions: HashMap<String, Vec<Reaction>>,
    /// Thread root id → reply count this device holds.
    pub thread_counts: HashMap<String, i64>,
    /// The highlighted message by id (ids survive a page merge). `None` follows the newest.
    pub selected: Option<String>,
    /// `Some` while a thread is open in the pane.
    pub thread: Option<ThreadView>,
}

impl OpenConversation {
    /// The messages the pane shows, in display order. The channel view hides
    /// thread replies (the root shows a count); a thread view is root plus replies.
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

    /// A loaded message by id, from the channel buffer or the open thread.
    pub fn find(&self, id: &str) -> Option<&ChannelMessage> {
        self.messages
            .iter()
            .chain(self.thread.iter().flat_map(|t| t.replies.iter()))
            .find(|m| m.id == id)
    }

    /// The selected message, if it is still loaded.
    pub fn selected_message(&self) -> Option<&ChannelMessage> {
        self.selected.as_deref().and_then(|id| self.find(id))
    }

    /// Move the message selection (+1 newer, -1 older), skipping lost-message placeholders.
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

/// Pure core of [`OpenConversation::step_selection`].
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

/// The whole Home screen model.
#[derive(Debug, Default)]
pub struct HomeState {
    pub tree: Option<ConversationTree>,
    /// Group ids whose channels are currently shown.
    pub expanded: HashSet<String>,
    /// Flattened sidebar, rebuilt whenever the tree or `expanded` changes.
    pub rows: Vec<SidebarRow>,
    /// Index into `rows` of the highlighted row.
    pub selected: usize,
    pub focus: Focus,
    /// The conversation shown in the main pane, if any.
    pub open: Option<OpenConversation>,
    /// Advances once per background-sync refresh — drives the sync spinner.
    pub refreshes: u64,
    /// How the screen is currently consuming keystrokes (nav / compose / prompt).
    pub mode: HomeMode,
    /// What the compose bar is for (new / reply / edit) while composing.
    pub compose: ComposeTarget,
}

impl HomeState {
    pub fn new() -> Self {
        Self {
            focus: Focus::Sidebar,
            ..Default::default()
        }
    }

    /// Replace the tree and rebuild the sidebar, keeping selection; groups start expanded.
    pub fn set_tree(&mut self, tree: ConversationTree, user_id: &str) {
        if self.tree.is_none() {
            for group in &tree.groups {
                self.expanded.insert(group.id.clone());
            }
        }
        self.tree = Some(tree);
        self.rebuild_rows(user_id);
    }

    /// Recompute `rows` and clamp the selection onto a selectable row.
    pub fn rebuild_rows(&mut self, user_id: &str) {
        self.rows = match &self.tree {
            Some(tree) => build_sidebar_rows(tree, &self.expanded, user_id),
            None => Vec::new(),
        };
        self.selected = clamp_selection(&self.rows, self.selected);
    }

    /// The currently-highlighted row, if any.
    pub fn current_row(&self) -> Option<&SidebarRow> {
        self.rows.get(self.selected)
    }

    /// Move the sidebar highlight by `dir` (+1 down, -1 up), skipping headers.
    pub fn move_selection(&mut self, dir: i32) {
        self.selected = step_selection(&self.rows, self.selected, dir);
    }

    /// Highlight the row that opens `conv_id`, to land on a just-created one (#1337).
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

    /// The group that scopes "new channel" / "invite" for the selection, if any.
    pub fn context_group(&self) -> Option<(String, String)> {
        let tree = self.tree.as_ref()?;
        context_group(&self.rows, tree, self.selected)
    }
}

/// The group heading above `selected`, stopping (`None`) at a section header.
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

/// Flatten the tree into sidebar rows: groups, then Direct Messages, then Requests.
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

/// Title a DM by the member who is not the current user.
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

/// Clamp `selected` to the nearest selectable row at or after it, else the last
/// selectable row, else 0.
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

/// Move `from` by `dir` to the next selectable row, staying put at the ends.
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

/// The bottom-anchored render window: `(start, end, top_pad)`, with `scroll` clamped.
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

/// Whether to prefetch the next older page.
pub fn should_load_older(scroll: usize, loaded: usize, has_more: bool, loading: bool) -> bool {
    has_more && !loading && scroll + PREFETCH_MARGIN >= loaded
}

/// Merge a newest-first page into the oldest-first buffer; the incoming copy wins.
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
        assert_eq!(rows.len(), 5);
        assert!(rows[0].label.starts_with("▸"));
    }

    #[test]
    fn dm_label_prefers_the_other_member() {
        let d = dm("d", "bob", Some("bob"));
        assert_eq!(dm_label(&d, "me"), "bob");
        let anon = dm("d", "xyz", None);
        assert_eq!(dm_label(&anon, "me"), "xyz");
    }

    #[test]
    fn step_selection_skips_headers_and_sticks_at_ends() {
        let t = tree();
        let mut expanded = HashSet::new();
        expanded.insert("g1".to_string());
        let rows = build_sidebar_rows(&t, &expanded, "me");
        assert_eq!(step_selection(&rows, 0, 1), 1);
        assert_eq!(step_selection(&rows, 2, 1), 4);
        assert_eq!(step_selection(&rows, 0, -1), 0);
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
        assert_eq!(clamp_selection(&rows, 0), 1);
        assert_eq!(clamp_selection(&rows, 99), 1);
        assert_eq!(clamp_selection(&[], 3), 0);
    }

    #[test]
    fn visible_window_bottom_anchors_and_clamps_scroll() {
        assert_eq!(visible_window(3, 10, 0), (0, 3, 7));
        assert_eq!(visible_window(100, 10, 0), (90, 100, 0));
        assert_eq!(visible_window(100, 10, 5), (85, 95, 0));
        assert_eq!(visible_window(100, 10, 999), (0, 10, 0));
        assert_eq!(visible_window(5, 0, 0), (5, 5, 0));
    }

    #[test]
    fn should_load_older_fires_only_near_the_top_with_more_history() {
        assert!(should_load_older(46, 50, true, false));
        assert!(!should_load_older(10, 50, true, false));
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
        // Rows: 0 group, 1 #welcome, 2 #random, 3 DMs, 4 @bob, 5 Requests, 6 @eve.
        home.selected = 6;
        assert_eq!(home.selected_dm_request().as_deref(), Some("r1"));
        home.selected = 4;
        assert_eq!(home.selected_dm_request(), None);
        home.selected = 1;
        assert_eq!(home.selected_dm_request(), None);
    }

    #[test]
    fn context_group_resolves_from_headings_and_channels_but_not_dms() {
        let mut home = HomeState::new();
        home.set_tree(tree(), "me");
        home.selected = 0;
        assert_eq!(
            home.context_group(),
            Some(("g1".to_string(), "General".to_string()))
        );
        home.selected = 2;
        assert_eq!(
            home.context_group(),
            Some(("g1".to_string(), "General".to_string()))
        );
        home.selected = 4;
        assert_eq!(home.context_group(), None);
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
        let incoming = vec![
            msg("c", "3", "third"),
            msg("a", "1", "first"),
            msg("b", "2", "second-edited"),
        ];
        let merged = merge_messages(existing, incoming);
        let ids: Vec<&str> = merged.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
        assert_eq!(merged[1].content.as_deref(), Some("second-edited"));
    }
}
