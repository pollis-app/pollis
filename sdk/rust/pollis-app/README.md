# pollis-app

Write a [Pollis](https://pollis.com) App in Rust.

```rust
use pollis_app::{Action, Command, CommandSpec, Context, Manifest};

fn on_command(cmd: Command, ctx: &Context) -> pollis_app::Result {
    ctx.reply(&cmd.message, format!("Hello, {}!", cmd.message.author.display_name))
}

pollis_app::app! {
    manifest: Manifest::new()
        .channels(["*"])
        .actions([Action::Reply])
        .command(CommandSpec::new("hello", "Say hello")),
    on_command: on_command,
}
```

Start with `npx pollis app new hello-bot --lang rust`. The walkthrough is the
[developer guide](https://github.com/pollis-app/pollis/blob/main/docs/bots/developer-guide.md).
