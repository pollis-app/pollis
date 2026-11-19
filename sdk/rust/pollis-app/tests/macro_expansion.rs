//! `app!` as an App uses it, compiled natively: what `pollis app build` reads
//! from it, and the handlers it accepts.

use pollis_app::{Action, Command, CommandSpec, Context, Manifest, Schedule, ScheduleSpec};

fn on_command(_: Command, _: &Context) -> pollis_app::Result {
    Ok(())
}

fn on_schedule(_: Schedule, _: &Context) -> pollis_app::Result {
    Ok(())
}

pollis_app::app! {
    manifest: Manifest::new()
        .channels(["*"])
        .actions([Action::Reply])
        .command(CommandSpec::new("hello", "Say hello"))
        .schedule(ScheduleSpec::new("daily", 86400)),
    on_command: on_command,
    on_schedule: on_schedule,
    on_mention: |message, ctx| ctx.reply(&message, "you rang"),
}

#[test]
fn describes_the_manifest_the_handlers_and_the_package() {
    let described = pollis_app::__rt::describe(
        &__pollis_app_manifest(),
        &__POLLIS_APP_HANDLERS,
        ["hello-bot", "1.2.3", "Says hello"],
    );
    assert_eq!(
        described,
        serde_json::json!({
            "manifest": {
                "read": { "channels": ["*"] },
                "actions": ["reply"],
                "commands": [{ "name": "hello", "description": "Say hello" }],
                "schedules": [{ "name": "daily", "every": 86400 }],
            },
            "handlers": ["onMention", "onCommand", "onSchedule"],
            "package": { "name": "hello-bot", "version": "1.2.3", "description": "Says hello" },
        })
    );
}
