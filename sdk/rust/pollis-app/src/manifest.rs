//! The manifest: what an App asks for. Written in code, inside
//! [`app!`](crate::app), and emitted as `pollis-app.json` by `pollis app
//! build`, so it cannot drift from what the code expects. An admin can narrow
//! it when approving the App, never widen it, and the runtime enforces it.
//!
//! This module only records what you asked for. `pollis app build` fills in
//! the defaults and checks every rule, then the runtime's own parser checks
//! the emitted file again.

use serde::Serialize;

/// What can wake an App. Each maps to one handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// `on_message`.
    Messages,
    /// `on_mention`.
    Mentions,
    /// `on_command`.
    Commands,
    /// `on_schedule`.
    Schedule,
    /// `on_request`.
    Requests,
    /// `on_webhook`.
    Webhook,
}

/// What an App may do in the group. There are no moderation actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Post,
    Reply,
    React,
    EditOwn,
    DeleteOwn,
    TransferHistory,
}

/// Message event kinds an App may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Created,
    Edited,
}

/// A slash command members can pick in the composer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSpec {
    name: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    arg_hint: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    request: bool,
}

impl CommandSpec {
    /// `name` is typed after the slash: lowercase letters, digits, '-' and
    /// '_', starting with a letter, up to 32 characters. `description` is one
    /// line, up to 100 characters, shown beside the name.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> CommandSpec {
        CommandSpec {
            name: name.into(),
            description: description.into(),
            arg_hint: None,
            request: false,
        }
    }

    /// What to type after the name, shown greyed (`"<title>"`). Leave it out
    /// for a command without arguments.
    pub fn arg_hint(mut self, hint: impl Into<String>) -> CommandSpec {
        self.arg_hint = Some(hint.into());
        self
    }

    /// Deliver privately to the bot (`on_request`, trigger `requests`) instead
    /// of posting in the channel (`on_command`).
    pub fn request(mut self) -> CommandSpec {
        self.request = true;
        self
    }
}

/// When `on_schedule` runs: at every Unix time `k * every + offset` (UTC seconds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduleSpec {
    name: String,
    every: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
}

impl ScheduleSpec {
    /// `name` is lowercase letters, digits and '-', up to 32 characters, and
    /// is passed to `on_schedule`. `every` is the period in seconds, 60 (a
    /// minute) to 604800 (a week).
    pub fn new(name: impl Into<String>, every: u64) -> ScheduleSpec {
        ScheduleSpec {
            name: name.into(),
            every,
            offset: None,
        }
    }

    /// Seconds after each period boundary, less than `every`. Default 0.
    pub fn offset(mut self, seconds: u64) -> ScheduleSpec {
        self.offset = Some(seconds);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct Read {
    #[serde(skip_serializing_if = "Option::is_none")]
    channels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    events: Option<Vec<EventKind>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct State {
    quota: u64,
}

/// What an App asks for. Every field is optional:
///
/// ```
/// use pollis_app::{Action, CommandSpec, Manifest, ScheduleSpec};
///
/// let manifest = Manifest::new()
///     .channels(["#support"])
///     .history(7)
///     .actions([Action::Reply, Action::React])
///     .state_quota(64 * 1024)
///     .egress(["api.linear.app"])
///     .command(CommandSpec::new("bug", "File a bug in Linear").arg_hint("<title>"))
///     .schedule(ScheduleSpec::new("digest", 86400).offset(32400));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Manifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    triggers: Option<Vec<Trigger>>,
    read: Read,
    #[serde(skip_serializing_if = "Option::is_none")]
    actions: Option<Vec<Action>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<State>,
    #[serde(skip_serializing_if = "Option::is_none")]
    egress: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    commands: Vec<CommandSpec>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    schedules: Vec<ScheduleSpec>,
}

fn strings<S: Into<String>>(values: impl IntoIterator<Item = S>) -> Vec<String> {
    values.into_iter().map(Into::into).collect()
}

impl Manifest {
    /// An empty manifest: no channels, no actions, no state, no egress.
    pub fn new() -> Manifest {
        Manifest::default()
    }

    /// Lowercase letters, digits and '-'. Default: the package name.
    pub fn name(mut self, name: impl Into<String>) -> Manifest {
        self.name = Some(name.into());
        self
    }

    /// Default: the package version.
    pub fn version(mut self, version: impl Into<String>) -> Manifest {
        self.version = Some(version.into());
        self
    }

    /// Default: the package description.
    pub fn description(mut self, description: impl Into<String>) -> Manifest {
        self.description = Some(description.into());
        self
    }

    /// Default: one trigger per handler you define. If given, it must match
    /// your handlers.
    pub fn triggers(mut self, triggers: impl IntoIterator<Item = Trigger>) -> Manifest {
        self.triggers = Some(triggers.into_iter().collect());
        self
    }

    /// `"#name"` for one channel, or `"*"` for whichever channels the admin grants.
    pub fn channels<S: Into<String>>(mut self, channels: impl IntoIterator<Item = S>) -> Manifest {
        self.read.channels = Some(strings(channels));
        self
    }

    /// Days of history `ctx.messages()` may reach back. Default 0: none.
    pub fn history(mut self, days: u32) -> Manifest {
        self.read.history = Some(days);
        self
    }

    /// Default `[EventKind::Created]`. Add `EventKind::Edited` to also see edits.
    pub fn events(mut self, events: impl IntoIterator<Item = EventKind>) -> Manifest {
        self.read.events = Some(events.into_iter().collect());
        self
    }

    /// What the App may do in the group.
    pub fn actions(mut self, actions: impl IntoIterator<Item = Action>) -> Manifest {
        self.actions = Some(actions.into_iter().collect());
        self
    }

    /// Bytes of keys plus values `ctx.state` may hold. Default 0: no state.
    pub fn state_quota(mut self, bytes: u64) -> Manifest {
        self.state = Some(State { quota: bytes });
        self
    }

    /// Exact host names `ctx.fetch` may reach. Empty: the App cannot send data
    /// outside the group.
    pub fn egress<S: Into<String>>(mut self, hosts: impl IntoIterator<Item = S>) -> Manifest {
        self.egress = Some(strings(hosts));
        self
    }

    /// Declare a slash command (up to 25). Needed for `on_command`; a
    /// [`request`](CommandSpec::request) command needs `on_request`.
    pub fn command(mut self, command: CommandSpec) -> Manifest {
        self.commands.push(command);
        self
    }

    /// Declare when `on_schedule` runs. Needed for `on_schedule`.
    pub fn schedule(mut self, schedule: ScheduleSpec) -> Manifest {
        self.schedules.push(schedule);
        self
    }
}
