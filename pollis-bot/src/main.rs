//! `pollis-bot`: the Pollis bot runtime (#1182). See `pollis-bot/README.md`.

use std::process::ExitCode;

use pollis_bot::cli::{self, Command};
use pollis_bot::config::{self, RuntimeConfig};
use pollis_bot::core_host::CoreHost;
use pollis_bot::host::PairTarget;
use pollis_bot::{app, log};

fn main() -> ExitCode {
    let invocation = match cli::parse(std::env::args().skip(1)) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("pollis-bot: {e} (see `pollis-bot --help`)");
            return ExitCode::from(2);
        }
    };
    match invocation.command {
        Command::Help => {
            println!("{}", cli::help_text());
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            println!("{}", cli::version_text());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    // Multi-thread: pollis-core's DB and keystore paths use spawn_blocking.
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("pollis-bot: starting the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let data_dir = config::data_dir(invocation.data_dir);
    match runtime.block_on(dispatch(&data_dir, invocation.command)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::failure("pollis-bot", format!("{e:#}"), "see the message above");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(data_dir: &std::path::Path, command: Command) -> anyhow::Result<()> {
    let config = RuntimeConfig::load(data_dir)?;
    // The core's own files (the keystore file, where there is no OS keychain) go in this data
    // dir, never the `pollis` app's: that file has no cross-process lock, so a runtime and the
    // desktop app, or two runtimes, must never share one.
    std::fs::create_dir_all(data_dir)?;
    pollis_core::set_data_dir_override(data_dir);
    let host = CoreHost::new(data_dir)?;
    match command {
        Command::Help | Command::Version => {}
        Command::Pair { name } => {
            app::pair(&host, data_dir, &config, PairTarget::NewBot, name).await?;
        }
        Command::AddHost { bot_user_id, name } => {
            app::pair(&host, data_dir, &config, PairTarget::ExistingBot { bot_user_id }, name).await?;
        }
        Command::Run { webhook, max_installations, once } => {
            let mut config = config;
            if let Some(max) = max_installations {
                config.max_installations = max;
            }
            let webhook = app::effective_webhook(&config, webhook);
            app::run(&host, data_dir, &config, webhook, once).await?;
        }
        Command::Post { installation_id, channel, text } => {
            let answer = app::post(&host, data_dir, &config, &installation_id, &channel, &text).await?;
            println!("{answer}");
        }
        Command::Status => print!("{}", app::status(data_dir, &config)?),
        Command::Forget { installation_id } => {
            app::forget(&host, data_dir, &installation_id).await?;
            println!("Installation {installation_id} removed from this host.");
        }
        Command::AppInstall { installation_id, artifact } => {
            app::app_install(data_dir, &installation_id, &artifact)?;
            println!("App installed for installation {installation_id}. Restart `pollis-bot run` to start it.");
        }
        Command::RotateSecret { installation_id } => {
            let secret = app::rotate_secret(&host, data_dir, &installation_id).await?;
            println!("New webhook secret for installation {installation_id} (shown once; restart `pollis-bot run` to use it):\n    {secret}");
        }
    }
    Ok(())
}
