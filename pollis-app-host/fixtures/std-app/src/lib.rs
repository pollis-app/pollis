//! An ordinary std Rust App for pollis-app-host's WASI tests (#2004). Its
//! handlers run a `;`-separated script from the trigger, like the no_std
//! fixture, and report each step through `log`.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

wit_bindgen::generate!({
    path: "../../../pollis-app-wit/wit",
    world: "app",
});

use pollis::app::host;
use pollis::app::types::LogLevel;

fn say(text: String) {
    host::log(LogLevel::Info, &text);
}

fn run(script: &str) -> Result<(), String> {
    for step in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (op, arg) = step.split_once(' ').unwrap_or((step, ""));
        match op {
            "print" => {
                println!("{arg}");
                eprintln!("err {arg}");
                say(format!("ok print"));
            }
            "env" => say(format!(
                "ok env: vars={} args={} cwd={:?}",
                std::env::vars().count(),
                std::env::args().count(),
                std::env::current_dir().ok()
            )),
            "time" => {
                let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
                say(format!("ok time: {ms}"));
            }
            "sleep" => {
                let start = Instant::now();
                std::thread::sleep(Duration::from_millis(arg.parse().unwrap_or(10)));
                say(format!("ok sleep: {}", start.elapsed().as_millis() >= arg.parse().unwrap_or(10)));
            }
            "hash" => {
                let mut m = HashMap::new();
                m.insert("k", 1);
                say(format!("ok hash: {}", m.len()));
            }
            "fs-read" => match std::fs::read(arg) {
                Ok(b) => say(format!("ok fs-read: {} bytes", b.len())),
                Err(e) => say(format!("err fs-read: {e}")),
            },
            "fs-write" => match std::fs::write(arg, b"x") {
                Ok(()) => say("ok fs-write".to_string()),
                Err(e) => say(format!("err fs-write: {e}")),
            },
            "fs-dir" => match std::fs::read_dir(arg) {
                Ok(d) => say(format!("ok fs-dir: {} entries", d.count())),
                Err(e) => say(format!("err fs-dir: {e}")),
            },
            "post" => {
                let (channel, text) = arg.split_once(' ').unwrap_or((arg, ""));
                match host::post(channel, text) {
                    Ok(()) => say(format!("ok {step}")),
                    Err(e) => say(format!("err {step}: {e}")),
                }
            }
            "exit" => std::process::exit(0),
            "panic" => panic!("boom"),
            other => say(format!("err {other}: unknown step")),
        }
    }
    Ok(())
}

struct StdApp;

impl Guest for StdApp {
    fn on_message(message: Message) -> Result<(), String> {
        run(&message.text)
    }
    fn on_mention(message: Message) -> Result<(), String> {
        run(&message.text)
    }
    fn on_command(command: Command) -> Result<(), String> {
        run(&command.args)
    }
    fn on_schedule(schedule: Schedule) -> Result<(), String> {
        run(&schedule.name)
    }
    fn on_request(request: Request) -> Result<(), String> {
        run(&request.body)
    }
    fn on_webhook(webhook: Webhook) -> Result<(), String> {
        run(&String::from_utf8_lossy(&webhook.body))
    }
}

export!(StdApp);
