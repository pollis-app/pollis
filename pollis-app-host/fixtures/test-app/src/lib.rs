//! The fixture App for pollis-app-host's tests (#2004).
//!
//! Every handler reads a tiny script from its trigger (a message's text, a
//! command's args, a schedule's name, a request's body, a webhook's body) and
//! runs it: steps separated by `;`, each a host call or a misbehaviour. It
//! reports each step's result through `log`, so a test can see exactly what the
//! host allowed and refused.
//!
//! `no_std` with a bump allocator so the component imports nothing but
//! `pollis:app/host`: no WASI at all. Memory is never freed, which is fine for
//! an instance that lives for one job.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

wit_bindgen::generate!({
    path: "../../../pollis-app-wit/wit",
    world: "app",
});

use pollis::app::host;
use pollis::app::types::{HttpRequest, LogLevel, MessageFilter};

struct Bump {
    next: UnsafeCell<usize>,
    end: UnsafeCell<usize>,
}

// SAFETY: the component model gives an instance a single thread.
unsafe impl Sync for Bump {}

const PAGE: usize = 65536;

unsafe impl GlobalAlloc for Bump {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let next = &mut *self.next.get();
        let end = &mut *self.end.get();
        let start = (*next + layout.align() - 1) & !(layout.align() - 1);
        let new_next = start + layout.size();
        if new_next > *end {
            let pages = (new_next - *end).div_ceil(PAGE).max(16);
            let prev = core::arch::wasm32::memory_grow(0, pages);
            if prev == usize::MAX {
                return core::ptr::null_mut();
            }
            if *end != prev * PAGE {
                // First growth, or someone else grew memory: start fresh.
                let base = prev * PAGE;
                *end = base + pages * PAGE;
                let start = (base + layout.align() - 1) & !(layout.align() - 1);
                *next = start + layout.size();
                return start as *mut u8;
            }
            *end += pages * PAGE;
        }
        *next = new_next;
        start as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOC: Bump = Bump {
    next: UnsafeCell::new(0),
    end: UnsafeCell::new(0),
};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

/// `no_std` on wasm32-wasip2 links no libc, and compiler_builtins leaves
/// `memcmp` to it (memcpy and memset lower to bulk-memory instructions).
///
/// # Safety
/// `a` and `b` must be valid for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y {
            return x as i32 - y as i32;
        }
    }
    0
}

/// The canonical ABI's allocator entry point. wit-bindgen supplies one only
/// with its `std`-flavoured `realloc` feature, so this `no_std` guest exports
/// its own over the bump allocator.
///
/// # Safety
/// Called by the host per the canonical ABI.
#[no_mangle]
pub unsafe extern "C" fn cabi_realloc(
    old: *mut u8,
    old_len: usize,
    align: usize,
    new_len: usize,
) -> *mut u8 {
    if new_len == 0 {
        return align as *mut u8;
    }
    let layout = Layout::from_size_align_unchecked(new_len, align);
    let new = ALLOC.alloc(layout);
    if new.is_null() {
        core::arch::wasm32::unreachable();
    }
    if !old.is_null() && old_len > 0 {
        core::ptr::copy_nonoverlapping(old, new, old_len.min(new_len));
    }
    new
}

/// Survives only as long as the instance; a fresh instance per job starts at 0.
static mut COUNTER: u32 = 0;

fn say(text: String) {
    host::log(LogLevel::Info, &text);
}

fn report<T>(step: &str, result: Result<T, String>, ok: impl FnOnce(T) -> String) {
    match result {
        Ok(v) => say(format!("ok {step}: {}", ok(v))),
        Err(e) => say(format!("err {step}: {e}")),
    }
}

fn kv_text(key: &str) -> String {
    match host::kv_get(key) {
        Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        Ok(None) => "none".to_string(),
        Err(e) => format!("error: {e}"),
    }
}

fn run(trigger: &str, script: &str) -> Result<(), String> {
    run_on(trigger, None, script)
}

fn run_on(trigger: &str, message: Option<&Message>, script: &str) -> Result<(), String> {
    let message_id = message.map(|m| m.id.as_str());
    say(format!("trigger {trigger}"));
    for step in script.split(';') {
        let step = step.trim();
        if step.is_empty() {
            continue;
        }
        let mut parts = step.splitn(3, ' ');
        let op = parts.next().unwrap_or("");
        let a = parts.next().unwrap_or("");
        let b = parts.next().unwrap_or("");
        match op {
            "post" => report(step, host::post(a, b), |_| String::new()),
            "reply" => report(step, host::reply(a, b), |_| String::new()),
            "react" => report(step, host::react(a, b), |_| String::new()),
            "edit" => report(step, host::edit_own(a, b), |_| String::new()),
            "delete" => report(step, host::delete_own(a), |_| String::new()),
            "query" => {
                let filter = MessageFilter {
                    channel: if a.is_empty() { None } else { Some(a.to_string()) },
                    since: b.parse().ok(),
                    until: None,
                    limit: 100,
                };
                report(step, host::query_messages(&filter), |msgs| {
                    msgs.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(",")
                });
            }
            "kv-get" => report(step, host::kv_get(a), |v| match v {
                Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                None => "none".to_string(),
            }),
            "kv-set" => report(step, host::kv_set(a, b.as_bytes()), |_| String::new()),
            "kv-del" => report(step, host::kv_delete(a), |_| String::new()),
            "fetch" => {
                let request = HttpRequest {
                    method: "GET".to_string(),
                    url: a.to_string(),
                    headers: Vec::new(),
                    body: None,
                };
                report(step, host::http_fetch(&request), |r| {
                    format!("{} {}", r.status, String::from_utf8_lossy(&r.body))
                });
            }
            "now" => say(format!("ok now: {}", host::now())),
            "random" => say(format!("ok random: {}", host::random())),
            "calls" => {
                let n: u32 = a.parse().unwrap_or(0);
                for _ in 0..n {
                    host::now();
                }
                say(format!("ok calls: {n}"));
            }
            // Read a counter, add one, write it back (state shared across hosts).
            "incr" => {
                let n: u64 = kv_text(a).parse().unwrap_or(0) + 1;
                report(step, host::kv_set(a, format!("{n}").as_bytes()), |_| format!("{n}"));
            }
            // Post "kv <key>=<value>" to a channel.
            "post-kv" => {
                let text = format!("kv {b}={}", kv_text(b));
                report(step, host::post(a, &text), |_| text.clone());
            }
            // Post the trigger message's author as the App sees it.
            "post-author" => match message {
                Some(m) => {
                    let text = format!("author {} bot={}", m.author.display_name, m.author.is_bot);
                    report(step, host::post(a, &text), |_| text.clone());
                }
                None => say(format!("err {step}: no trigger message")),
            },
            // Reply to the trigger's own message.
            "ack" => match message_id {
                Some(id) => {
                    let text = format!("{a} {b}").trim().to_string();
                    report(step, host::reply(id, &text), |_| String::new());
                }
                None => say(format!("err {step}: no trigger message")),
            },
            "static" => {
                // SAFETY: single-threaded instance.
                let value = unsafe {
                    COUNTER += 1;
                    COUNTER
                };
                say(format!("ok static: {value}"));
            }
            "fail" => return Err(format!("{a} {b}").trim().to_string()),
            "trap" => core::arch::wasm32::unreachable(),
            "spin" => loop {
                core::hint::black_box(());
            },
            "oom" => {
                let mut hoard: Vec<Vec<u8>> = Vec::new();
                loop {
                    hoard.push(alloc::vec![1u8; 1 << 20]);
                }
            }
            other => say(format!("err {other}: unknown step")),
        }
    }
    Ok(())
}

struct TestApp;

impl Guest for TestApp {
    fn on_message(message: Message) -> Result<(), String> {
        run_on(&format!("message {}", message.id), Some(&message), &message.text)
    }

    fn on_mention(message: Message) -> Result<(), String> {
        run_on(&format!("mention {}", message.id), Some(&message), &message.text)
    }

    fn on_command(command: Command) -> Result<(), String> {
        run(&format!("command {} {}", command.name, command.message.id), &command.args)
    }

    fn on_schedule(schedule: Schedule) -> Result<(), String> {
        run(&format!("schedule {}", schedule.slot), &schedule.name)
    }

    fn on_request(request: Request) -> Result<(), String> {
        run(&format!("request {}", request.id), &request.body)
    }

    fn on_webhook(webhook: Webhook) -> Result<(), String> {
        let body = String::from_utf8_lossy(&webhook.body).into_owned();
        run(&format!("webhook {}", webhook.delivery_id), &body)
    }
}

export!(TestApp);
