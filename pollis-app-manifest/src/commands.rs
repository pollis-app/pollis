//! Slash commands (#2018): what an App declares under `commands`, and how a
//! message's text is read as a command.
//!
//! **Wire format.** A command is an ordinary channel message whose text starts
//! with `/<name>`, optionally followed by the target bot's `@username` and then
//! the arguments: `/ticket @linear_bot printer is on fire`. Nothing new crosses
//! the wire, so a client that knows nothing about commands shows it as a normal
//! message. The composer adds the `@username` when two installed Apps declare
//! the same name, so routing never has to guess.
//!
//! **Routing** ([`route_command`]) is the runtime's, never the App's: a message
//! reaches an installation's `on-command` only when the name is declared in
//! that installation's approved manifest (not as a `request`), the App takes
//! the `commands` trigger, and any target named is this bot. The channel check
//! is the sandbox host's, like every other message trigger.
//!
//! A command declared with `request: true` is never routed from a channel
//! message: the composer delivers it to the bot as a sealed request (#2008)
//! instead of posting it.

use serde::{Deserialize, Serialize};

use crate::{Manifest, TriggerKind};

/// Most commands one App may declare.
pub const MAX_COMMANDS: usize = 25;
/// Longest command name, in characters.
pub const MAX_COMMAND_NAME_LEN: usize = 32;
/// Longest one-line description, in characters.
pub const MAX_COMMAND_DESCRIPTION_LEN: usize = 100;
/// Longest argument hint, in characters.
pub const MAX_COMMAND_ARG_HINT_LEN: usize = 60;

/// One entry of the manifest's `commands`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// Typed after the slash: lowercase letters, digits, '-' and '_', starting
    /// with a letter, at most [`MAX_COMMAND_NAME_LEN`] characters.
    pub name: String,
    /// One line the composer shows beside the name.
    pub description: String,
    /// What to type after the name, shown greyed in the composer
    /// (`"<title>"`, `"[days]"`). Absent: the command takes no arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_hint: Option<String>,
    /// Deliver as a sealed request to the bot (`on-request`) instead of
    /// posting it in the channel (`on-command`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub request: bool,
}

/// Whether `name` is a valid command name.
pub fn valid_command_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_COMMAND_NAME_LEN
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

/// A message's text read as a command. Borrowed from the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedCommand<'a> {
    /// The name as typed after the slash, before lowercasing; compare with
    /// [`ParsedCommand::is`].
    pub name: &'a str,
    /// The bot's username (without '@') when the text names one right after
    /// the command.
    pub target: Option<&'a str>,
    /// Everything after the name and the target, trimmed.
    pub args: &'a str,
}

impl ParsedCommand<'_> {
    /// Whether this is the command `name` (case-insensitive).
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// Whether the target, if any, is the bot `username` (case-insensitive).
    /// No target reads as addressed to every App declaring the name.
    pub fn addressed_to(&self, username: Option<&str>) -> bool {
        match (self.target, username) {
            (None, _) => true,
            (Some(t), Some(u)) => t.eq_ignore_ascii_case(u),
            (Some(_), None) => false,
        }
    }
}

/// Read `text` as a command: a slash first (leading whitespace ignored), a
/// valid name ending at whitespace or the end, then an optional `@username`.
/// `None` for anything else, including `/` alone, `//`, `/ name` and a path
/// like `/usr/bin`.
pub fn parse_command(text: &str) -> Option<ParsedCommand<'_>> {
    let rest = text.trim_start().strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    if !valid_command_name(&name.to_ascii_lowercase()) {
        return None;
    }
    let after = rest[end..].trim_start();
    let token_end = after.find(char::is_whitespace).unwrap_or(after.len());
    let (target, args) = match after[..token_end].strip_prefix('@') {
        Some(username) if !username.is_empty() => (Some(username), &after[token_end..]),
        _ => (None, after),
    };
    Some(ParsedCommand { name, target, args: args.trim() })
}

/// The text the composer sends for a picked command: `/name`, then
/// `@target` when the composer had to disambiguate, then the arguments.
pub fn command_text(name: &str, target: Option<&str>, args: &str) -> String {
    let mut out = format!("/{name}");
    if let Some(t) = target {
        out.push_str(" @");
        out.push_str(t);
    }
    let args = args.trim();
    if !args.is_empty() {
        out.push(' ');
        out.push_str(args);
    }
    out
}

/// A message routed to `on-command`: the declared name and the arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedCommand {
    pub name: String,
    pub args: String,
}

/// Whether a channel message is a command for this installation, by the
/// rules in the module docs. `own_username` is the bot's username in the
/// group (`None` if the roster read had no username for it: then only
/// untargeted commands reach it). The channel is checked by the sandbox host.
pub fn route_command(manifest: &Manifest, text: &str, own_username: Option<&str>) -> Option<RoutedCommand> {
    if !manifest.triggers.contains(&TriggerKind::Commands) {
        return None;
    }
    let parsed = parse_command(text)?;
    if !parsed.addressed_to(own_username) {
        return None;
    }
    let spec = manifest.command(parsed.name)?;
    if spec.request {
        return None;
    }
    Some(RoutedCommand { name: spec.name.clone(), args: parsed.args.to_string() })
}

impl Manifest {
    /// The declared command called `name` (case-insensitive), if any.
    pub fn command(&self, name: &str) -> Option<&CommandSpec> {
        self.commands.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }
}

/// The `commands` checks [`Manifest::validate`] runs.
pub(crate) fn validate_commands(manifest: &Manifest) -> Result<(), crate::ManifestError> {
    use crate::ManifestError::Command;
    if manifest.commands.len() > MAX_COMMANDS {
        return Err(Command(format!("at most {MAX_COMMANDS} commands")));
    }
    let mut seen = std::collections::BTreeSet::new();
    for c in &manifest.commands {
        if !valid_command_name(&c.name) {
            return Err(Command(format!(
                "name {:?} is not valid: lowercase letters, digits, '-' and '_', 1 to {MAX_COMMAND_NAME_LEN} characters, starting with a letter",
                c.name
            )));
        }
        if !seen.insert(c.name.as_str()) {
            return Err(Command(format!("/{} is declared twice", c.name)));
        }
        let description = c.description.trim();
        if description.is_empty() || description.chars().count() > MAX_COMMAND_DESCRIPTION_LEN || description.contains('\n') {
            return Err(Command(format!(
                "/{}: `description` is one line of 1 to {MAX_COMMAND_DESCRIPTION_LEN} characters",
                c.name
            )));
        }
        if let Some(hint) = &c.arg_hint {
            if hint.trim().is_empty() || hint.chars().count() > MAX_COMMAND_ARG_HINT_LEN || hint.contains('\n') {
                return Err(Command(format!(
                    "/{}: `arg_hint` is one line of 1 to {MAX_COMMAND_ARG_HINT_LEN} characters (leave it out for no arguments)",
                    c.name
                )));
            }
        }
        let needs = if c.request { TriggerKind::Requests } else { TriggerKind::Commands };
        if !manifest.triggers.contains(&needs) {
            return Err(Command(format!(
                "/{} needs `{}` in `triggers`{}",
                c.name,
                needs.as_str(),
                if c.request { " (it is a `request` command)" } else { "" }
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(commands: &str, triggers: &str) -> Result<Manifest, crate::ManifestError> {
        Manifest::from_json(
            format!(r#"{{"name":"linear","version":"1","triggers":[{triggers}],"commands":[{commands}]}}"#).as_bytes(),
        )
    }

    fn ticket() -> Manifest {
        manifest(
            r#"{"name":"ticket","description":"File a ticket","arg_hint":"<title>"},
               {"name":"forget-me","description":"Ask for your data","request":true}"#,
            r#""commands","requests""#,
        )
        .unwrap()
    }

    #[test]
    fn parses_name_target_and_args() {
        let p = parse_command("/ticket @Linear_bot printer is on fire ").unwrap();
        assert!(p.is("ticket"));
        assert_eq!(p.target, Some("Linear_bot"));
        assert_eq!(p.args, "printer is on fire");

        let p = parse_command("  /Ticket").unwrap();
        assert!(p.is("ticket"));
        assert_eq!((p.target, p.args), (None, ""));

        let p = parse_command("/ticket printer @alice").unwrap();
        assert_eq!((p.target, p.args), (None, "printer @alice"), "a later mention is an argument");
    }

    #[test]
    fn ordinary_text_is_not_a_command() {
        for text in ["", "/", "//", "/ ticket", "hello /ticket", "/usr/bin", "/1abc", "/ti!cket x", "ticket"] {
            assert_eq!(parse_command(text), None, "{text:?}");
        }
    }

    #[test]
    fn command_text_round_trips() {
        let text = command_text("ticket", Some("linear_bot"), " printer ");
        assert_eq!(text, "/ticket @linear_bot printer");
        let p = parse_command(&text).unwrap();
        assert_eq!((p.name, p.target, p.args), ("ticket", Some("linear_bot"), "printer"));
        assert_eq!(command_text("ticket", None, ""), "/ticket");
    }

    #[test]
    fn routes_only_declared_non_request_commands() {
        let m = ticket();
        assert_eq!(
            route_command(&m, "/ticket fix it", Some("linear_bot")),
            Some(RoutedCommand { name: "ticket".into(), args: "fix it".into() })
        );
        assert_eq!(route_command(&m, "/deploy now", Some("linear_bot")), None, "undeclared");
        assert_eq!(route_command(&m, "/forget-me", Some("linear_bot")), None, "a request is never routed from a channel");
        assert_eq!(route_command(&m, "file a /ticket", Some("linear_bot")), None);
    }

    #[test]
    fn a_target_routes_to_that_bot_only() {
        let m = ticket();
        assert!(route_command(&m, "/ticket @linear_bot x", Some("linear_bot")).is_some());
        assert!(route_command(&m, "/ticket @LINEAR_BOT x", Some("linear_bot")).is_some());
        assert_eq!(route_command(&m, "/ticket @jira_bot x", Some("linear_bot")), None);
        assert_eq!(route_command(&m, "/ticket @jira_bot x", None), None);
        assert!(route_command(&m, "/ticket x", None).is_some(), "untargeted reaches every declaring App");
    }

    #[test]
    fn without_the_commands_trigger_nothing_routes() {
        let mut m = ticket();
        m.triggers.remove(&TriggerKind::Commands);
        m.commands.retain(|c| c.request);
        assert_eq!(route_command(&m, "/forget-me", None), None);
    }

    #[test]
    fn validation_names_the_problem() {
        let bad = [
            (r#"{"name":"Ticket","description":"d"}"#, r#""commands""#, "not valid"),
            (r#"{"name":"9lives","description":"d"}"#, r#""commands""#, "not valid"),
            (r#"{"name":"t","description":""}"#, r#""commands""#, "`description`"),
            (r#"{"name":"t","description":"d","arg_hint":""}"#, r#""commands""#, "`arg_hint`"),
            (r#"{"name":"t","description":"d"},{"name":"t","description":"e"}"#, r#""commands""#, "declared twice"),
            (r#"{"name":"t","description":"d"}"#, r#""messages""#, "needs `commands`"),
            (r#"{"name":"t","description":"d","request":true}"#, r#""commands""#, "needs `requests`"),
        ];
        for (commands, triggers, why) in bad {
            let err = manifest(commands, triggers).unwrap_err().to_string();
            assert!(err.contains(why), "{commands}: {err}");
        }
        let unknown = manifest(r#"{"name":"t","description":"d","hidden":true}"#, r#""commands""#);
        assert!(matches!(unknown, Err(crate::ManifestError::Parse(_))));
        let many: Vec<String> = (0..=MAX_COMMANDS).map(|i| format!(r#"{{"name":"c{i}","description":"d"}}"#)).collect();
        assert!(manifest(&many.join(","), r#""commands""#).is_err());
    }

    #[test]
    fn defaults_are_left_out_when_serialized() {
        let v = serde_json::to_value(&ticket().commands).unwrap();
        assert_eq!(v[0], serde_json::json!({"name":"ticket","description":"File a ticket","arg_hint":"<title>"}));
        assert_eq!(v[1]["request"], true);
    }
}
