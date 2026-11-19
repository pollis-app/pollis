//! What a handler can do: [`Context`], and the runtime's host functions behind it.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::bindings::pollis::app::{host, types as wit};
use crate::error::{Error, Result};
use crate::types::{Attachment, Author, Message, MessageRef};

fn refused<T>(result: std::result::Result<T, String>) -> Result<T> {
    result.map_err(Error::refused)
}

pub(crate) fn author(a: wit::Author) -> Author {
    Author {
        id: a.id,
        display_name: a.display_name,
        is_bot: a.is_bot,
    }
}

pub(crate) fn message(m: wit::Message) -> Message {
    Message {
        id: m.id,
        channel: m.channel,
        author: author(m.author),
        text: m.text,
        attachments: m
            .attachments
            .into_iter()
            .map(|a| Attachment {
                name: a.name,
                mime_type: a.mime_type,
                size: a.size,
            })
            .collect(),
        reply_to: m.reply_to,
        thread: m.thread,
        sent_at: m.sent_at,
        edited_at: m.edited_at,
    }
}

/// What `ctx.messages()` returns. All fields optional:
/// `MessageQuery { limit: Some(10), ..Default::default() }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageQuery {
    /// One channel id; all granted channels when absent.
    pub channel: Option<String>,
    /// Unix milliseconds, inclusive. Clamped to the history window the App was granted.
    pub since: Option<u64>,
    /// Unix milliseconds, exclusive.
    pub until: Option<u64>,
    /// At most this many (default 50; the runtime may return fewer).
    pub limit: Option<u32>,
}

/// The request half of `ctx.fetch`. Default: a `GET` with no headers or body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchInit {
    method: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

impl FetchInit {
    /// A `GET` with no headers or body.
    pub fn new() -> FetchInit {
        FetchInit::default()
    }

    /// The HTTP method, in any case.
    pub fn method(mut self, method: impl Into<String>) -> FetchInit {
        self.method = Some(method.into());
        self
    }

    /// Add a request header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> FetchInit {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The request body: text or bytes.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> FetchInit {
        self.body = Some(body.into());
        self
    }

    /// The request body as JSON, with `content-type: application/json` unless
    /// you set a content type yourself.
    pub fn json<T: Serialize + ?Sized>(mut self, value: &T) -> Result<FetchInit> {
        self.body = Some(serde_json::to_vec(value)?);
        if !self
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        {
            self.headers
                .push(("content-type".to_string(), "application/json".to_string()));
        }
        Ok(self)
    }
}

/// Response headers. Names compare case-insensitively.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchHeaders(Vec<(String, String)>);

impl FetchHeaders {
    /// A header's value; repeated headers are joined with `, `.
    pub fn get(&self, name: &str) -> Option<String> {
        let values: Vec<&str> = self
            .0
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect();
        if values.is_empty() {
            None
        } else {
            Some(values.join(", "))
        }
    }

    /// Whether the response has this header.
    pub fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Every header as `(lowercase name, value)`, in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// The response half of `ctx.fetch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResponse {
    /// The HTTP status.
    pub status: u16,
    /// The response headers.
    pub headers: FetchHeaders,
    body: Vec<u8>,
}

impl FetchResponse {
    /// `status` is 200 to 299.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The body as UTF-8 text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body parsed as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    /// The body's bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.body
    }
}

/// The installation's key-value state: sealed, shared by every host that runs
/// the bot, limited to the manifest's `state_quota` bytes. It is the only
/// thing that survives between jobs. Writes are kept only if the handler
/// returns `Ok`.
#[derive(Debug)]
pub struct State(());

impl State {
    /// A key's bytes.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        refused(host::kv_get(key))
    }

    /// A key's value as UTF-8 text.
    pub fn get_text(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .get(key)?
            .map(|value| String::from_utf8_lossy(&value).into_owned()))
    }

    /// A key's value parsed as JSON.
    pub fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.get(key)? {
            Some(value) => Ok(Some(serde_json::from_slice(&value)?)),
            None => Ok(None),
        }
    }

    /// Set a key to text or bytes.
    pub fn set(&self, key: &str, value: impl AsRef<[u8]>) -> Result {
        refused(host::kv_set(key, value.as_ref()))
    }

    /// Set a key to a value as JSON.
    pub fn set_json<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result {
        self.set(key, serde_json::to_vec(value)?)
    }

    /// Delete a key.
    pub fn delete(&self, key: &str) -> Result {
        refused(host::kv_delete(key))
    }
}

/// The runtime's log (`pollis-bot` output). Never shown in the group.
#[derive(Debug)]
pub struct Logger(());

impl Logger {
    pub fn trace(&self, text: impl AsRef<str>) {
        host::log(wit::LogLevel::Trace, text.as_ref());
    }

    pub fn debug(&self, text: impl AsRef<str>) {
        host::log(wit::LogLevel::Debug, text.as_ref());
    }

    pub fn info(&self, text: impl AsRef<str>) {
        host::log(wit::LogLevel::Info, text.as_ref());
    }

    pub fn warn(&self, text: impl AsRef<str>) {
        host::log(wit::LogLevel::Warn, text.as_ref());
    }

    pub fn error(&self, text: impl AsRef<str>) {
        host::log(wit::LogLevel::Error, text.as_ref());
    }
}

/// What a handler can do. Every call is checked against what the admin
/// granted; a refused call returns an [`Error`] whose
/// [`is_refused`](Error::is_refused) is true and whose message names the
/// manifest field.
///
/// Messages you post, reply, react, edit or delete are queued, and sent only
/// after the handler returns `Ok`. If it returns an error, nothing is sent and
/// no state is written.
#[derive(Debug)]
pub struct Context {
    /// The installation's key-value state. Needs `state_quota` above 0.
    pub state: State,
    /// The runtime's log.
    pub log: Logger,
}

impl Context {
    pub(crate) fn new() -> Context {
        Context {
            state: State(()),
            log: Logger(()),
        }
    }

    /// Post a new message in a granted channel, by id (e.g. `message.channel`)
    /// or by name (`"support"` or `"#support"`, any case). Needs `Action::Post`.
    pub fn post(&self, channel: impl AsRef<str>, text: impl AsRef<str>) -> Result {
        refused(host::post(channel.as_ref(), text.as_ref()))
    }

    /// Reply to a message. Needs `Action::Reply`.
    pub fn reply(&self, to: impl MessageRef, text: impl AsRef<str>) -> Result {
        refused(host::reply(to.message_id(), text.as_ref()))
    }

    /// React to a message with an emoji. Needs `Action::React`.
    pub fn react(&self, to: impl MessageRef, emoji: impl AsRef<str>) -> Result {
        refused(host::react(to.message_id(), emoji.as_ref()))
    }

    /// Edit a message this bot sent. Needs `Action::EditOwn`.
    pub fn edit_own(&self, message: impl MessageRef, text: impl AsRef<str>) -> Result {
        refused(host::edit_own(message.message_id(), text.as_ref()))
    }

    /// Delete a message this bot sent. Needs `Action::DeleteOwn`.
    pub fn delete_own(&self, message: impl MessageRef) -> Result {
        refused(host::delete_own(message.message_id()))
    }

    /// Messages from granted channels within the granted history window.
    pub fn messages(&self, query: MessageQuery) -> Result<Vec<Message>> {
        let filter = wit::MessageFilter {
            channel: query.channel,
            since: query.since,
            until: query.until,
            limit: query.limit.unwrap_or(50),
        };
        Ok(refused(host::query_messages(&filter))?
            .into_iter()
            .map(message)
            .collect())
    }

    /// One HTTPS request to a host listed in `egress`. Redirects are followed
    /// only within the same host.
    pub fn fetch(&self, url: impl AsRef<str>, init: FetchInit) -> Result<FetchResponse> {
        let request = wit::HttpRequest {
            method: init.method.unwrap_or_else(|| "GET".to_string()).to_uppercase(),
            url: url.as_ref().to_string(),
            headers: init.headers,
            body: init.body,
        };
        let response = refused(host::http_fetch(&request))?;
        Ok(FetchResponse {
            status: response.status,
            headers: FetchHeaders(
                response
                    .headers
                    .into_iter()
                    .map(|(name, value)| (name.to_lowercase(), value))
                    .collect(),
            ),
            body: response.body,
        })
    }

    /// Unix milliseconds, from the runtime's clock.
    pub fn now(&self) -> u64 {
        host::now()
    }

    /// A cryptographically random value.
    pub fn random(&self) -> u64 {
        host::random()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_sets_a_content_type_unless_one_was_given() {
        let plain = FetchInit::new().json(&1).unwrap();
        assert_eq!(plain.headers, [("content-type".to_string(), "application/json".to_string())]);
        assert_eq!(plain.body.as_deref(), Some(&b"1"[..]));
        let custom = FetchInit::new()
            .header("Content-Type", "application/vnd.api+json")
            .json(&1)
            .unwrap();
        assert_eq!(custom.headers.len(), 1);
    }

    #[test]
    fn response_headers_join_repeats_and_ignore_case() {
        let headers = FetchHeaders(vec![
            ("x-tag".to_string(), "a".to_string()),
            ("x-tag".to_string(), "b".to_string()),
        ]);
        assert_eq!(headers.get("X-Tag").as_deref(), Some("a, b"));
        assert!(headers.has("x-tag"));
        assert!(!headers.has("x-other"));
        assert_eq!(headers.iter().count(), 2);
    }

    #[test]
    fn a_response_is_ok_from_200_to_299() {
        let response = |status| FetchResponse {
            status,
            headers: FetchHeaders::default(),
            body: b"[1]".to_vec(),
        };
        assert!(response(200).ok());
        assert!(response(299).ok());
        assert!(!response(300).ok());
        assert!(!response(199).ok());
        assert_eq!(response(200).text(), "[1]");
        assert_eq!(response(200).json::<Vec<u8>>().unwrap(), [1]);
    }
}
