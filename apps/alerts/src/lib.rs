//! The alerts App: a webhook call with the JSON body `{"channel", "text"}`
//! becomes one post in that channel. Anything else posts nothing.

use pollis_app::{Action, Context, Manifest, Webhook};
use serde::Deserialize;

/// What a sender posts. `channel` is a channel's name (with or without `#`, any
/// case) or its id.
#[derive(Deserialize)]
struct Alert {
    channel: String,
    text: String,
}

fn on_webhook(webhook: Webhook, ctx: &Context) -> pollis_app::Result {
    let Ok(alert) = webhook.json::<Alert>() else {
        pollis_app::bail!("bad_request: send a JSON body {{\"channel\": \"...\", \"text\": \"...\"}}");
    };
    ctx.post(alert.channel, alert.text)
}

pollis_app::app! {
    manifest: Manifest::new()
        // Whichever channels the admin picks when approving it.
        .channels(["*"])
        .actions([Action::Post]),
    on_webhook: on_webhook,
}
