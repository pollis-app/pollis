//! The host's own types: what the job loop hands in (a [`Trigger`]), what it
//! gets back (a [`JobOutcome`] or a [`JobError`]), and the [`HostServices`] it
//! implements. Kept free of wasmtime types so the runtime never names them.

use std::time::Duration;

/// Who a job runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationContext {
    /// The installation (one App approved into one group).
    pub installation_id: String,
    /// The bot's user id: messages it authored are the only ones `edit-own`
    /// and `delete-own` may touch, and are never delivered back to it.
    pub bot_user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub id: String,
    pub display_name: String,
    pub is_bot: bool,
}

/// Attachment metadata. Bytes are a later capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub mime_type: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub channel: String,
    pub author: Author,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<String>,
    pub thread: Option<String>,
    /// Unix milliseconds.
    pub sent_at: u64,
    /// Unix milliseconds of the last edit.
    pub edited_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub args: String,
    pub message: Message,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub name: String,
    /// Unix milliseconds.
    pub slot: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    pub requester: Author,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Webhook {
    pub delivery_id: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// One reason to run an App. The job loop's trigger key for claiming is the
/// message id, schedule slot, request id or webhook delivery id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    Message(Message),
    Mention(Message),
    Command(Command),
    Schedule(Schedule),
    Request(Request),
    Webhook(Webhook),
}

/// Something the App asked for, checked against the grants, to be applied by
/// the runtime only when the job returns ok. `channel` is always a granted
/// channel; for the message-addressed actions it is the target's channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Post {
        channel: String,
        text: String,
    },
    Reply {
        channel: String,
        message: String,
        text: String,
    },
    React {
        channel: String,
        message: String,
        emoji: String,
    },
    EditOwn {
        channel: String,
        message: String,
        text: String,
    },
    DeleteOwn {
        channel: String,
        message: String,
    },
}

/// A write to the installation's key-value state, to be committed (sealed)
/// by the runtime together with the actions. `None` deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvWrite {
    pub key: String,
    pub value: Option<Vec<u8>>,
}

/// A job that returned ok. Nothing has been applied yet: the runtime claims
/// the trigger, then applies `actions` and commits `kv_writes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobOutcome {
    pub actions: Vec<Action>,
    pub kv_writes: Vec<KvWrite>,
}

/// Why a job produced nothing. In every case the job's queued actions and
/// state writes are discarded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    /// The trigger is outside the installation's grants (an undeclared
    /// trigger, an ungranted channel or event kind, or the bot's own message).
    /// Not a failure: the App is never instantiated.
    #[error("not delivered: {0}")]
    NotDelivered(String),
    /// The admin's grants are wider than the manifest.
    #[error("grants refused: {0}")]
    GrantsRefused(String),
    /// The component bytes do not match what this App hash was compiled from.
    #[error("component does not match App hash {0}")]
    AppHashMismatch(String),
    /// The component did not compile or does not target `pollis:app@0.1.0`.
    #[error("component refused: {0}")]
    InvalidComponent(String),
    /// The component imports something the host does not provide.
    #[error("{0}")]
    ImportRefused(String),
    /// The job ran past its CPU deadline.
    #[error("job exceeded its {0:?} deadline")]
    Timeout(Duration),
    /// The App tried to grow memory past the cap.
    #[error("job exceeded its memory cap of {0} bytes")]
    MemoryExceeded(usize),
    /// The App called host functions more often than allowed.
    #[error("{0}")]
    HostCallLimit(String),
    /// The App trapped.
    #[error("App trapped: {0}")]
    Trap(String),
    /// The handler returned an error.
    #[error("App returned an error: {0}")]
    HandlerFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// What `query-messages` asks the runtime for, already narrowed to the grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageQuery {
    /// Granted channels only.
    pub channels: Vec<String>,
    /// Unix milliseconds, inclusive; never earlier than the history window.
    pub since: u64,
    pub until: Option<u64>,
    pub limit: u32,
}

/// What the host needs to know to check an action on an existing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageMeta {
    pub channel: String,
    pub author_id: String,
}

/// An outbound request that has passed the egress checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequest {
    pub method: String,
    /// Absolute `https://` URL on a granted host, default port.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A host service failed (a network error, a store error). Reported to the
/// App as the error of the host call; it does not trap the job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError(pub String);

/// What the runtime provides to the sandbox. The host calls these only after
/// a call has passed every grant check, and filters what comes back again.
///
/// Calls are synchronous: the job loop runs [`crate::AppHost::run_job`] on a
/// blocking thread, and an async runtime implements these with its handle's
/// `block_on`.
pub trait HostServices: Send + Sync {
    /// The installation's committed value for `key`.
    fn kv_get(&self, installation_id: &str, key: &str) -> Result<Option<Vec<u8>>, ServiceError>;

    /// Bytes (keys plus values) the installation's committed state holds.
    fn kv_usage(&self, installation_id: &str) -> Result<u64, ServiceError>;

    /// Messages matching `query` from the installation's unlocked history.
    /// Before the runtime keeps a message log (#2008) this returns the current
    /// trigger's context only.
    fn query_messages(
        &self,
        installation_id: &str,
        query: &MessageQuery,
    ) -> Result<Vec<Message>, ServiceError>;

    /// Channel and author of a message the installation can see, if any.
    fn lookup_message(
        &self,
        installation_id: &str,
        message_id: &str,
    ) -> Result<Option<MessageMeta>, ServiceError>;

    /// Perform exactly one HTTPS request. Must not follow redirects (the host
    /// checks and follows same-host ones itself) and must stop reading the
    /// body after `max_body_bytes + 1` bytes.
    fn http_fetch(
        &self,
        request: &OutboundRequest,
        max_body_bytes: usize,
    ) -> Result<HttpResponse, ServiceError>;

    /// The runtime's log. Never the group.
    fn log(&self, installation_id: &str, level: LogLevel, text: &str);

    /// Unix milliseconds.
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Per-job limits. Exceeding the deadline, memory cap or any call cap traps
/// the job and discards everything it queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLimits {
    /// Wall-clock deadline for the whole job, enforced by epoch interruption.
    pub deadline: Duration,
    /// Linear memory, summed over the App's memories.
    pub memory_bytes: usize,
    /// Every host call counts, including `log`, `now` and `random`.
    pub max_host_calls: u32,
    pub max_http_fetches: u32,
    pub max_queries: u32,
    pub max_actions: u32,
    pub max_http_response_bytes: usize,
    /// Same-host redirects followed per `http-fetch`.
    pub max_redirects: u32,
    /// Messages returned by one `query-messages`.
    pub max_query_results: u32,
    pub max_text_bytes: usize,
    pub max_kv_key_bytes: usize,
    pub max_log_bytes: usize,
}

impl Default for JobLimits {
    fn default() -> Self {
        JobLimits {
            deadline: Duration::from_secs(10),
            memory_bytes: 64 << 20,
            max_host_calls: 10_000,
            max_http_fetches: 16,
            max_queries: 16,
            max_actions: 32,
            max_http_response_bytes: 1 << 20,
            max_redirects: 3,
            max_query_results: 500,
            max_text_bytes: 16 << 10,
            max_kv_key_bytes: 256,
            max_log_bytes: 4 << 10,
        }
    }
}
