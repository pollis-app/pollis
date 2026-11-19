use pollis_app::{Action, Command, CommandSpec, Context, Manifest};

// Runs when a member sends one of the commands declared below, e.g. "/hello Sam".
fn on_command(cmd: Command, ctx: &Context) -> pollis_app::Result {
    if cmd.name != "hello" {
        return Ok(());
    }
    let who = match cmd.args.trim() {
        "" => cmd.message.author.display_name.as_str(),
        name => name,
    };
    ctx.reply(&cmd.message, format!("Hello, {who}!"))
}

pollis_app::app! {
    manifest: Manifest::new()
        // Where the App works. "*" means: whichever channels the admin picks
        // when approving it. Use "#name" to ask for one channel by name.
        .channels(["*"])
        // What the App may do there. The admin sees this list before approving.
        .actions([Action::Reply])
        // No `.egress(...)`: this App cannot send anything outside the group.
        // The slash commands members can pick in the composer.
        .command(CommandSpec::new("hello", "Say hello").arg_hint("[name]")),
    on_command: on_command,
}
