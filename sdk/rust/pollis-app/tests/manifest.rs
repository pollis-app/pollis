//! The manifest as the CLI receives it: only what the App asked for, spelled
//! as `pollis-app.json` spells it.

use pollis_app::{Action, CommandSpec, EventKind, Manifest, ScheduleSpec, Trigger};
use serde_json::json;

#[test]
fn an_empty_manifest_asks_for_nothing() {
    assert_eq!(serde_json::to_value(Manifest::new()).unwrap(), json!({ "read": {} }));
}

#[test]
fn every_field_is_spelled_as_the_manifest_file_spells_it() {
    let manifest = Manifest::new()
        .name("linear-bot")
        .version("2.0.0")
        .description("Files bugs")
        .triggers([Trigger::Commands, Trigger::Requests, Trigger::Schedule])
        .channels(["#support"])
        .history(7)
        .events([EventKind::Created, EventKind::Edited])
        .actions([Action::Post, Action::EditOwn, Action::DeleteOwn, Action::TransferHistory])
        .state_quota(1024)
        .egress(["api.linear.app"])
        .command(CommandSpec::new("bug", "File a bug").arg_hint("<title>"))
        .command(CommandSpec::new("secret", "Tell the bot").request())
        .schedule(ScheduleSpec::new("digest", 86400).offset(32400));
    assert_eq!(
        serde_json::to_value(manifest).unwrap(),
        json!({
            "name": "linear-bot",
            "version": "2.0.0",
            "description": "Files bugs",
            "triggers": ["commands", "requests", "schedule"],
            "read": { "channels": ["#support"], "history": 7, "events": ["created", "edited"] },
            "actions": ["post", "edit_own", "delete_own", "transfer_history"],
            "state": { "quota": 1024 },
            "egress": ["api.linear.app"],
            "commands": [
                { "name": "bug", "description": "File a bug", "arg_hint": "<title>" },
                { "name": "secret", "description": "Tell the bot", "request": true },
            ],
            "schedules": [{ "name": "digest", "every": 86400, "offset": 32400 }],
        })
    );
}
