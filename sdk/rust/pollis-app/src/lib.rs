//! `pollis-app`: write a Pollis App in Rust.
//!
//! An App is a call to [`app!`]: a manifest saying what it needs, and handlers
//! the bot runtime calls when something happens in the group. `pollis app
//! build` compiles it into a sandboxed WebAssembly component and writes the
//! manifest beside it as `pollis-app.json`.
//!
//! ```no_run
//! use pollis_app::{Action, Command, CommandSpec, Context, Manifest};
//!
//! fn on_command(cmd: Command, ctx: &Context) -> pollis_app::Result {
//!     ctx.reply(&cmd.message, format!("Hello, {}!", cmd.message.author.display_name))
//! }
//!
//! pollis_app::app! {
//!     manifest: Manifest::new()
//!         .channels(["*"])
//!         .actions([Action::Reply])
//!         .command(CommandSpec::new("hello", "Say hello")),
//!     on_command: on_command,
//! }
//! # fn main() {}
//! ```
//!
//! The handlers are `on_message`, `on_mention`, `on_command`, `on_schedule`,
//! `on_request` and `on_webhook`; define the ones you need. Each job runs in a
//! fresh instance, so nothing in memory survives from one job to the next:
//! keep what must last in [`Context::state`].
//!
//! [`testing`] runs the built App offline, in your tests.

mod context;
mod error;
mod manifest;
#[cfg(not(target_arch = "wasm32"))]
pub mod testing;
mod types;

pub use context::{Context, FetchHeaders, FetchInit, FetchResponse, Logger, MessageQuery, State};
pub use error::{Error, Result};
pub use manifest::{Action, CommandSpec, EventKind, Manifest, ScheduleSpec, Trigger};
pub use types::{
    Attachment, Author, Command, Message, MessageRef, Request, Schedule, Webhook,
};

// The generated `http-request`-sized host calls take more arguments than clippy likes.
#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "app",
        pub_export_macro: true,
        export_macro_name: "__export_app",
        default_bindings_module: "pollis_app::bindings",
    });
}

/// What [`app!`] expands to. Not part of the API.
#[doc(hidden)]
pub mod __rt {
    use crate::bindings::pollis::app::types as wit;
    use crate::context::{author, message};
    use crate::{Command, Context, Manifest, Message, Request, Result, Schedule, Webhook};

    pub use crate::bindings::Guest;
    pub use wit::{
        Command as WitCommand, Message as WitMessage, Request as WitRequest,
        Schedule as WitSchedule, Webhook as WitWebhook,
    };

    type Handler<T> = fn(T, &Context) -> Result;

    /// The handlers an App defined.
    #[derive(Clone, Copy)]
    pub struct Handlers {
        pub on_message: Option<Handler<Message>>,
        pub on_mention: Option<Handler<Message>>,
        pub on_command: Option<Handler<Command>>,
        pub on_schedule: Option<Handler<Schedule>>,
        pub on_request: Option<Handler<Request>>,
        pub on_webhook: Option<Handler<Webhook>>,
    }

    impl Handlers {
        pub const NONE: Handlers = Handlers {
            on_message: None,
            on_mention: None,
            on_command: None,
            on_schedule: None,
            on_request: None,
            on_webhook: None,
        };

        /// The defined handlers, by the names the CLI's manifest rules use.
        fn names(&self) -> Vec<&'static str> {
            let defined = [
                ("onMessage", self.on_message.is_some()),
                ("onMention", self.on_mention.is_some()),
                ("onCommand", self.on_command.is_some()),
                ("onSchedule", self.on_schedule.is_some()),
                ("onRequest", self.on_request.is_some()),
                ("onWebhook", self.on_webhook.is_some()),
            ];
            defined
                .into_iter()
                .filter(|(_, is_defined)| *is_defined)
                .map(|(name, _)| name)
                .collect()
        }
    }

    fn run<T>(name: &str, handler: Option<Handler<T>>, arg: T) -> std::result::Result<(), String> {
        let Some(handler) = handler else {
            return Err(format!("the App has no {name} handler"));
        };
        handler(arg, &Context::new()).map_err(|e| e.to_string())
    }

    pub fn on_message(h: &Handlers, m: WitMessage) -> std::result::Result<(), String> {
        run("on_message", h.on_message, message(m))
    }

    pub fn on_mention(h: &Handlers, m: WitMessage) -> std::result::Result<(), String> {
        run("on_mention", h.on_mention, message(m))
    }

    pub fn on_command(h: &Handlers, c: WitCommand) -> std::result::Result<(), String> {
        let command = Command {
            name: c.name,
            args: c.args,
            message: message(c.message),
        };
        run("on_command", h.on_command, command)
    }

    pub fn on_schedule(h: &Handlers, s: WitSchedule) -> std::result::Result<(), String> {
        let schedule = Schedule {
            name: s.name,
            slot: s.slot,
        };
        run("on_schedule", h.on_schedule, schedule)
    }

    pub fn on_request(h: &Handlers, r: WitRequest) -> std::result::Result<(), String> {
        let request = Request {
            id: r.id,
            requester: author(r.requester),
            body: r.body,
        };
        run("on_request", h.on_request, request)
    }

    pub fn on_webhook(h: &Handlers, w: WitWebhook) -> std::result::Result<(), String> {
        let webhook = Webhook {
            delivery_id: w.delivery_id,
            headers: w.headers,
            body: w.body,
        };
        run("on_webhook", h.on_webhook, webhook)
    }

    /// What `pollis app build` reads: the manifest as written, the handlers
    /// and the package's defaults. The CLI resolves and checks it with the
    /// same code it uses for a TypeScript App.
    pub fn describe(
        manifest: &Manifest,
        handlers: &Handlers,
        package: [&str; 3],
    ) -> serde_json::Value {
        let [name, version, description] = package;
        serde_json::json!({
            "manifest": manifest,
            "handlers": handlers.names(),
            "package": { "name": name, "version": version, "description": description },
        })
    }

    /// Write [`describe`]'s output where `pollis app build` asked for it.
    /// Does nothing under a plain `cargo test`.
    pub fn emit(description: serde_json::Value) {
        if let Some(path) = std::env::var_os("POLLIS_APP_DESCRIBE_OUT") {
            std::fs::write(path, description.to_string()).expect("writing the App's manifest");
        }
    }
}

/// Define the App: its manifest, then the handlers it has. Call it once, at
/// the root of the crate.
///
/// ```no_run
/// use pollis_app::{Action, Context, Manifest, Message};
///
/// fn on_message(message: Message, ctx: &Context) -> pollis_app::Result {
///     if message.text.trim() == "ping" {
///         ctx.reply(&message, "pong")?;
///     }
///     Ok(())
/// }
///
/// pollis_app::app! {
///     manifest: Manifest::new().channels(["*"]).actions([Action::Reply]),
///     on_message: on_message,
/// }
/// # fn main() {}
/// ```
#[macro_export]
macro_rules! app {
    (manifest: $manifest:expr $(, $handler:ident : $function:expr)* $(,)?) => {
        #[doc(hidden)]
        const __POLLIS_APP_HANDLERS: $crate::__rt::Handlers = $crate::__rt::Handlers {
            $($handler: ::core::option::Option::Some($function),)*
            ..$crate::__rt::Handlers::NONE
        };

        #[doc(hidden)]
        #[allow(dead_code)]
        fn __pollis_app_manifest() -> $crate::Manifest {
            $manifest
        }

        #[doc(hidden)]
        struct __PollisApp;

        impl $crate::__rt::Guest for __PollisApp {
            fn on_message(m: $crate::__rt::WitMessage) -> ::core::result::Result<(), ::std::string::String> {
                $crate::__rt::on_message(&__POLLIS_APP_HANDLERS, m)
            }
            fn on_mention(m: $crate::__rt::WitMessage) -> ::core::result::Result<(), ::std::string::String> {
                $crate::__rt::on_mention(&__POLLIS_APP_HANDLERS, m)
            }
            fn on_command(c: $crate::__rt::WitCommand) -> ::core::result::Result<(), ::std::string::String> {
                $crate::__rt::on_command(&__POLLIS_APP_HANDLERS, c)
            }
            fn on_schedule(s: $crate::__rt::WitSchedule) -> ::core::result::Result<(), ::std::string::String> {
                $crate::__rt::on_schedule(&__POLLIS_APP_HANDLERS, s)
            }
            fn on_request(r: $crate::__rt::WitRequest) -> ::core::result::Result<(), ::std::string::String> {
                $crate::__rt::on_request(&__POLLIS_APP_HANDLERS, r)
            }
            fn on_webhook(w: $crate::__rt::WitWebhook) -> ::core::result::Result<(), ::std::string::String> {
                $crate::__rt::on_webhook(&__POLLIS_APP_HANDLERS, w)
            }
        }

        $crate::bindings::__export_app!(__PollisApp with_types_in $crate::bindings);

        // How `pollis app build` reads the manifest: this runs natively, so
        // the manifest never has to be compiled into the component.
        #[cfg(test)]
        #[test]
        fn __pollis_app_describe() {
            $crate::__rt::emit($crate::__rt::describe(
                &__pollis_app_manifest(),
                &__POLLIS_APP_HANDLERS,
                [
                    ::core::env!("CARGO_PKG_NAME"),
                    ::core::env!("CARGO_PKG_VERSION"),
                    ::core::env!("CARGO_PKG_DESCRIPTION"),
                ],
            ));
        }
    };
}
