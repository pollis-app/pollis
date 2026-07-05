//! The M2b three-pane client state — the interactive shell on top of the
//! already-gated read/sync layer (`pollis_tui::{data, sync}`).
//!
//! This module owns the pure, in-memory model of the Home screen: the flattened
//! sidebar (groups → channels + DMs + requests), which pane has focus, the open
//! conversation's message buffer, and the scroll/pagination bookkeeping. The
//! async work (loading the tree, loading a page of messages) lives in `app.rs`;
//! everything here is a **pure function of state** so the render stays an
//! immediate-mode projection and the tricky bits — tree flattening, selection
//! movement, scroll windowing, page merging — are unit-tested in isolation.

use std::collections::HashSet;

use pollis_core::commands::messages::{ChannelMessage, MessageCursor};

use pollis_tui::data::ConversationTree;

/// How close to the top of the loaded buffer the user must scroll before we
/// prefetch the next older page — keeps history loading a beat ahead of the eye.
pub const PREFETCH_MARGIN: usize = 5;

/// Which pane currently receives navigation keys.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Sidebar,
    Messages,
}

/// What a bottom-bar text prompt (a create/invite flow) is collecting. The
/// active buffer lives on [`crate::app::App::input`] so rendering reads one
/// field; this only records *which* action the collected text feeds and any
/// context (the target group) the submit needs. Kept a small pure enum so its
/// label + validation are unit-testable in isolation, like the rest of this
/// module.
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
        }
    }
}

/// How the Home screen is currently consuming keystrokes: navigating the tree,
/// composing a message for the open conversation, or filling a create/invite
/// prompt. The text buffer for the latter two lives on [`crate::app::App`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum HomeMode {
    /// Navigation + command keys (the default).
    #[default]
    Navigate,
    /// Typing a message into the open conversation.
    Compose,
    /// Filling a create/invite prompt (bottom input bar).
    Prompt(PromptKind),
}

/// Whether a collected buffer is worth submitting: non-empty after trimming.
/// Shared by compose (empty send is a no-op) and every prompt (empty submit is
/// rejected), so an all-whitespace value can never reach the write layer.
pub fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// The three kinds of thing the message pane can be showing, which decides
/// whether we read via `data::channel_messages` or `data::dm_messages`.
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
    /// Cursor for the next *older* page; `None` once history is exhausted or
    /// before the first load completes.
    pub older_cursor: Option<MessageCursor>,
    /// True once a page came back short — the beginning of history is loaded.
    pub at_beginning: bool,
    /// Lines scrolled up from the bottom (0 = pinned to newest).
    pub scroll: usize,
    /// True while the first page is still loading (drives the empty state).
    pub loading: bool,
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
}

impl HomeState {
    pub fn new() -> Self {
        Self {
            focus: Focus::Sidebar,
            ..Default::default()
        }
    }

    /// Replace the tree (from `data::load_conversations`) and rebuild the
    /// sidebar, preserving the selection and expansion where still valid. On the
    /// very first load, groups start expanded so the user immediately sees their
    /// channels.
    pub fn set_tree(&mut self, tree: ConversationTree, user_id: &str) {
        if self.tree.is_none() {
            for group in &tree.groups {
                self.expanded.insert(group.id.clone());
            }
        }
        self.tree = Some(tree);
        self.rebuild_rows(user_id);
    }

    /// Recompute `rows` from the current tree + expansion, then clamp the
    /// selection onto a still-selectable row.
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

    /// The id of the selected sidebar row iff it's a **pending DM request** —
    /// the target of the "accept" key. `None` for any other row.
    pub fn selected_dm_request(&self) -> Option<String> {
        match &self.current_row()?.target {
            RowTarget::Open(conv) if conv.kind == ConvKind::DmRequest => Some(conv.id.clone()),
            _ => None,
        }
    }

    /// The (id, name) of the group that scopes a "new channel" / "invite"
    /// action for the current selection: the nearest group heading at or above
    /// the highlighted row. Returns `None` if the selection sits in the DM /
    /// Requests sections (past a section header), where no group applies.
    pub fn context_group(&self) -> Option<(String, String)> {
        let tree = self.tree.as_ref()?;
        context_group(&self.rows, tree, self.selected)
    }
}

/// The group heading governing `selected`: walk up from the selection, resolving
/// the first [`RowTarget::ToggleGroup`] against `tree.groups`, but stop (→ `None`)
/// at a section header, so a DM/request selection has no group context. Pure so
/// the walk-up + boundary rules are unit-tested.
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
            // Crossed into the DM / Requests sections — no group applies.
            RowTarget::Header => return None,
            // A channel row under a group — keep walking up to its heading.
            RowTarget::Open(_) => {}
        }
    }
    None
}

/// Flatten a [`ConversationTree`] into the ordered list of sidebar rows given
/// which groups are expanded. Order: each group heading (its channels nested
/// when expanded), then a Direct-Messages section, then a Requests section.
/// `user_id` is used to title DMs by the *other* member.
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

/// Title a DM by the member who isn't the current user (falling back to a
/// generic label if the metadata is missing or it's a self-note).
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
/// selectable row, else 0. Keeps a rebuilt sidebar from landing on a header.
pub fn clamp_selection(rows: &[SidebarRow], selected: usize) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let start = selected.min(rows.len() - 1);
    // Search forward from the clamped position, then backward.
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

/// Move `from` by `dir` (+1/-1) to the next selectable row, staying put at the
/// ends. Headers are transparently skipped.
pub fn step_selection(rows: &[SidebarRow], from: usize, dir: i32) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let last = rows.len() as i32 - 1;
    let mut i = from as i32;
    loop {
        let next = i + dir;
        if next < 0 || next > last {
            // Past an end — keep the last valid selectable position.
            return i.clamp(0, last) as usize;
        }
        i = next;
        if rows[i as usize].selectable() {
            return i as usize;
        }
    }
}

/// The slice of a message buffer to render and how many blank lines to pad above
/// it, for a bottom-anchored list. Returns `(start, end, top_pad)`: render
/// `messages[start..end]` preceded by `top_pad` blank lines. `scroll` counts
/// lines up from the bottom and is clamped so it can never scroll past the top.
pub fn visible_window(total: usize, viewport: usize, scroll: usize) -> (usize, usize, usize) {
    if viewport == 0 {
        return (total, total, 0);
    }
    if total <= viewport {
        // Everything fits — pin it to the bottom by padding the top.
        return (0, total, viewport - total);
    }
    let max_scroll = total - viewport;
    let s = scroll.min(max_scroll);
    let end = total - s;
    let start = end - viewport;
    (start, end, 0)
}

/// Whether the user has scrolled close enough to the top of the loaded buffer
/// that we should prefetch the next older page.
pub fn should_load_older(scroll: usize, loaded: usize, has_more: bool, loading: bool) -> bool {
    has_more && !loading && scroll + PREFETCH_MARGIN >= loaded
}

/// Merge a freshly-fetched page (core order: newest-first) into an existing
/// oldest-first buffer, deduping by id (the incoming copy wins, so edits/
/// deletes overwrite) and re-sorting by `(sent_at, id)`. Uniform across a
/// newest-page refresh and an older-page fetch: both just contribute rows.
pub fn merge_messages(
    existing: Vec<ChannelMessage>,
    incoming: Vec<ChannelMessage>,
) -> Vec<ChannelMessage> {
    let mut all = existing;
    // Drop any existing rows the incoming page supersedes, then append.
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
            sent_at: sent_at.to_string(),
            edited_at: None,
            deleted_at: None,
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
        // Missing username falls back to the id; self-only DM gets a generic name.
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
