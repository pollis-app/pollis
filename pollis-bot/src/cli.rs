//! Argument parsing and help text for the `pollis-bot` binary.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::config::{DEFAULT_MAX_INSTALLATIONS, DEFAULT_WEBHOOK_BIND};
use crate::ratelimit::{DEFAULT_BURST, DEFAULT_PER_MINUTE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub data_dir: Option<PathBuf>,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Pair { name: Option<String> },
    AddHost { bot_user_id: String, name: Option<String> },
    Run { webhook: Option<Option<SocketAddr>>, max_installations: Option<usize> },
    Post { installation_id: String, channel: String, text: String },
    Status,
    RotateSecret { installation_id: String },
}

pub fn help_text() -> String {
    format!(
        "pollis-bot {version} — the Pollis bot runtime

USAGE
    pollis-bot [--data-dir DIR] <command> [options]

COMMANDS
    pair [--name NAME]
        Start a new bot on this host. Prints an eight-character code; a group
        admin types it in the group's settings → Bots → Add a bot.
    add-host --bot BOT_USER_ID [--name NAME]
        Make this host another host of an existing bot. Prints a code; an admin
        types it on the bot's page → Add a host.
    run [--webhook [ADDR]] [--max-installations N]
        Run every installation on this host until stopped. --webhook turns the
        alerts webhook on (default {bind}, loopback) for this run.
    post --installation ID --channel CHANNEL TEXT...
        Post TEXT through the running runtime's webhook endpoint.
    status
        Show this host's installations and what the runtime is doing.
    rotate-secret --installation ID
        Replace an installation's webhook secret and print the new one once.

FILES
    DIR/config.json      max_installations (default {max}), webhook {{bind, per_minute, burst}}
                         (default {per_minute}/minute, burst {burst}, per installation)
    DIR/installations/   one directory per installation
    DIR/status.json      written by `run`, read by `status`

ENVIRONMENT
    POLLIS_BOT_DATA_DIR  data directory when --data-dir is not given
",
        version = env!("CARGO_PKG_VERSION"),
        bind = DEFAULT_WEBHOOK_BIND,
        max = DEFAULT_MAX_INSTALLATIONS,
        per_minute = DEFAULT_PER_MINUTE,
        burst = DEFAULT_BURST,
    )
}

pub fn version_text() -> String {
    format!("pollis-bot {}", env!("CARGO_PKG_VERSION"))
}

/// Parse `args` (without the program name).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Invocation, String> {
    let mut args = args.into_iter().peekable();
    let mut data_dir = None;
    let mut command_name = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" | "help" => return Ok(Invocation { data_dir, command: Command::Help }),
            "-V" | "--version" => return Ok(Invocation { data_dir, command: Command::Version }),
            "--data-dir" => data_dir = Some(PathBuf::from(value(&mut args, "--data-dir")?)),
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
            other => {
                command_name = Some(other.to_string());
                break;
            }
        }
    }
    let Some(name) = command_name else {
        return Ok(Invocation { data_dir, command: Command::Help });
    };

    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--" {
            positional.extend(args.by_ref());
            break;
        }
        if arg == "-h" || arg == "--help" {
            return Ok(Invocation { data_dir, command: Command::Help });
        }
        if arg == "--data-dir" {
            data_dir = Some(PathBuf::from(value(&mut args, "--data-dir")?));
            continue;
        }
        if let Some(flag) = arg.strip_prefix("--") {
            // `--webhook` alone takes no value; every other flag takes one.
            let takes_value = flag != "webhook" || args.peek().is_some_and(|next| !next.starts_with('-') && next.contains(':'));
            let v = if takes_value { Some(value(&mut args, &arg)?) } else { None };
            flags.push((flag.to_string(), v));
        } else {
            positional.push(arg);
        }
    }

    let mut take = |flag: &str| -> Option<Option<String>> {
        let pos = flags.iter().position(|(f, _)| f == flag)?;
        Some(flags.remove(pos).1)
    };
    let command = match name.as_str() {
        "pair" => Command::Pair { name: take("name").flatten() },
        "add-host" => Command::AddHost {
            bot_user_id: take("bot").flatten().ok_or("add-host needs --bot BOT_USER_ID")?,
            name: take("name").flatten(),
        },
        "run" => {
            let webhook = match take("webhook") {
                None => None,
                Some(None) => Some(None),
                Some(Some(addr)) => {
                    Some(Some(addr.parse().map_err(|_| format!("--webhook `{addr}` is not an address like 127.0.0.1:8787"))?))
                }
            };
            let max_installations = match take("max-installations").flatten() {
                None => None,
                Some(n) => match n.parse::<usize>() {
                    Ok(n) if n > 0 => Some(n),
                    _ => return Err(format!("--max-installations `{n}` must be a number of at least 1")),
                },
            };
            Command::Run { webhook, max_installations }
        }
        "post" => {
            let installation_id = take("installation").flatten().ok_or("post needs --installation ID")?;
            let channel = take("channel").flatten().ok_or("post needs --channel CHANNEL")?;
            let text = positional.join(" ");
            positional.clear();
            if text.trim().is_empty() {
                return Err("post needs the text to send".into());
            }
            Command::Post { installation_id, channel, text }
        }
        "status" => Command::Status,
        "rotate-secret" => Command::RotateSecret {
            installation_id: take("installation").flatten().ok_or("rotate-secret needs --installation ID")?,
        },
        other => return Err(format!("unknown command `{other}`")),
    };
    if let Some((flag, _)) = flags.first() {
        return Err(format!("unknown option `--{flag}` for `{name}`"));
    }
    if let Some(extra) = positional.first() {
        return Err(format!("unexpected argument `{extra}` for `{name}`"));
    }
    Ok(Invocation { data_dir, command })
}

fn value<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Invocation, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn subcommands_parse() {
        assert_eq!(p(&["pair"]).unwrap().command, Command::Pair { name: None });
        assert_eq!(
            p(&["--data-dir", "/x", "add-host", "--bot", "01B", "--name", "rack-2"]).unwrap(),
            Invocation {
                data_dir: Some("/x".into()),
                command: Command::AddHost { bot_user_id: "01B".into(), name: Some("rack-2".into()) },
            }
        );
        assert_eq!(
            p(&["post", "--installation", "01I", "--channel", "#alerts", "disk", "full"]).unwrap().command,
            Command::Post { installation_id: "01I".into(), channel: "#alerts".into(), text: "disk full".into() }
        );
        assert_eq!(p(&["status"]).unwrap().command, Command::Status);
        assert_eq!(
            p(&["rotate-secret", "--installation", "01I"]).unwrap().command,
            Command::RotateSecret { installation_id: "01I".into() }
        );
    }

    #[test]
    fn run_takes_an_optional_webhook_address() {
        assert_eq!(p(&["run"]).unwrap().command, Command::Run { webhook: None, max_installations: None });
        assert_eq!(p(&["run", "--webhook"]).unwrap().command, Command::Run { webhook: Some(None), max_installations: None });
        assert_eq!(
            p(&["run", "--webhook", "0.0.0.0:9000", "--max-installations", "3"]).unwrap().command,
            Command::Run { webhook: Some(Some("0.0.0.0:9000".parse().unwrap())), max_installations: Some(3) }
        );
        assert_eq!(
            p(&["run", "--webhook", "--max-installations", "2"]).unwrap().command,
            Command::Run { webhook: Some(None), max_installations: Some(2) }
        );
    }

    #[test]
    fn mistakes_are_refused_with_what_was_wrong() {
        assert!(p(&["frobnicate"]).unwrap_err().contains("frobnicate"));
        assert!(p(&["add-host"]).unwrap_err().contains("--bot"));
        assert!(p(&["post", "--installation", "01I", "--channel", "c"]).unwrap_err().contains("text"));
        assert!(p(&["run", "--max-installations", "0"]).is_err());
        assert!(p(&["run", "--webhook", "nonsense:x"]).is_err());
        assert!(p(&["status", "--loud", "yes"]).unwrap_err().contains("--loud"));
        assert!(p(&["status", "extra"]).unwrap_err().contains("extra"));
        assert_eq!(p(&[]).unwrap().command, Command::Help);
    }
}
