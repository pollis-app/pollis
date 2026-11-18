//! A deterministic pre-initializer for ComponentizeJS builds (#2006).
//!
//! ComponentizeJS runs the StarlingMonkey engine's `componentize.wizer`
//! function once at build time and snapshots the resulting memory into the
//! component (Wizer). Stock Wizer gives that run real randomness and real
//! clocks, and SpiderMonkey seeds its hash tables from them, so two builds of
//! the same source differ and an App's hash could not be reproduced from its
//! source. This runs the same snapshot with fixed randomness and clocks, so
//! the same source and lockfile give the same bytes.
//!
//! It is invoked by ComponentizeJS as its `wizerBin`, with Wizer's own
//! arguments (`--allow-wasi --init-func … --dir … --mapdir G::H -o=OUT IN`)
//! and the engine's runtime arguments on stdin.
//!
//! The fixed randomness affects only build-time state: at run time the App
//! host supplies real randomness (`Math.random` and `crypto.getRandomValues`
//! differ per job; `tests/build.rs` in the CLI pins this).
//!
//! The WASI here (`wasmtime-wasi`, preview 1) exists only for this build step
//! on the developer's machine, with read-only preopens; the App host itself
//! links no WASI implementation beyond its own stubs.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use wasmtime::{Config, Engine, Extern, Instance, Linker, Module, Store, Val};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::p2::pipe::MemoryInputPipe;
use wasmtime_wasi::{Deterministic, FsPerms, HostMonotonicClock, HostWallClock, WasiCtxBuilder};
use wasmtime_wizer::{InstanceState, SnapshotVal, ValType};

/// Wall clock fixed at the Unix epoch.
struct FixedWall;

impl HostWallClock for FixedWall {
    fn resolution(&self) -> Duration {
        Duration::from_nanos(1)
    }
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

/// A monotonic clock that advances 1µs per reading, so a loop waiting for
/// time to pass still terminates, deterministically.
#[derive(Default)]
struct SteppingMonotonic(AtomicU64);

impl HostMonotonicClock for SteppingMonotonic {
    fn resolution(&self) -> u64 {
        1
    }
    fn now(&self) -> u64 {
        self.0.fetch_add(1_000, Ordering::Relaxed)
    }
}

/// Wizer's command line, as ComponentizeJS passes it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WizerArgs {
    pub init_func: String,
    pub dirs: Vec<String>,
    /// (guest, host)
    pub mapdirs: Vec<(String, String)>,
    pub output: String,
    pub input: String,
}

/// Whether `args` (without the program name) is a Wizer invocation.
pub fn is_wizer_invocation(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "--allow-wasi")
}

pub fn parse_args(args: &[String]) -> Result<WizerArgs, String> {
    let mut out = WizerArgs {
        init_func: "wizer.initialize".to_string(),
        ..WizerArgs::default()
    };
    let mut output = None;
    let mut input = None;
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--init-func" || arg == "-f" {
            out.init_func = value(&mut i, arg)?;
        } else if let Some(v) = arg.strip_prefix("--init-func=") {
            out.init_func = v.to_string();
        } else if arg == "--dir" {
            out.dirs.push(value(&mut i, arg)?);
        } else if let Some(v) = arg.strip_prefix("--dir=") {
            out.dirs.push(v.to_string());
        } else if arg == "--mapdir" || arg.starts_with("--mapdir=") {
            let spec = match arg.strip_prefix("--mapdir=") {
                Some(v) => v.to_string(),
                None => value(&mut i, arg)?,
            };
            let (guest, host) = spec
                .split_once("::")
                .ok_or_else(|| format!("--mapdir {spec:?} is not GUEST::HOST"))?;
            out.mapdirs.push((guest.to_string(), host.to_string()));
        } else if arg == "-o" {
            output = Some(value(&mut i, arg)?);
        } else if let Some(v) = arg.strip_prefix("-o=") {
            output = Some(v.to_string());
        } else if arg == "--allow-wasi"
            || arg.starts_with("--wasm-")
            || arg.starts_with("--inherit-")
        {
            // Always on (allow-wasi), the engine default (bulk memory), or
            // fixed below (environment: inherited from ComponentizeJS, which
            // passes only its own variables; stdio: inherited).
        } else if arg.starts_with('-') {
            return Err(format!("unsupported Wizer option {arg}"));
        } else {
            input = Some(arg.to_string());
        }
        i += 1;
    }
    out.output = output.ok_or("missing -o=<output>")?;
    out.input = input.ok_or("missing input module")?;
    Ok(out)
}

/// Run the pre-initialization and write the snapshot to `args.output`.
pub fn run(args: &WizerArgs, stdin: Vec<u8>) -> wasmtime::Result<()> {
    let mut builder = WasiCtxBuilder::new();
    builder
        .stdin(MemoryInputPipe::new(stdin))
        .inherit_stdout()
        .inherit_stderr()
        .secure_random(Deterministic::new(vec![0x5a; 32]))
        .insecure_random(Deterministic::new(vec![0xa5; 32]))
        .insecure_random_seed(0)
        .wall_clock(FixedWall)
        .monotonic_clock(SteppingMonotonic::default());
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.sort();
    for (k, v) in env {
        builder.env(k, v);
    }
    for dir in &args.dirs {
        builder.preopened_dir(dir, dir, FsPerms::ReadOnly)?;
    }
    for (guest, host) in &args.mapdirs {
        builder.preopened_dir(host, guest, FsPerms::ReadOnly)?;
    }
    let ctx: WasiP1Ctx = builder.build_p1();

    let mut config = Config::new();
    config.wasm_bulk_memory(true);
    let engine = Engine::new(&config)?;
    let mut store = Store::new(&engine, ctx);
    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_async(&mut linker, |ctx| ctx)?;

    let wasm = std::fs::read(&args.input)?;
    let mut wizer = wasmtime_wizer::Wizer::new();
    wizer.init_func(args.init_func.clone());
    let snapshot = block_on(snapshot(&wizer, &mut store, &linker, &wasm))?;
    std::fs::write(&args.output, snapshot)?;
    Ok(())
}

/// Wizer's own `Wizer::run` (its `wasmtime` feature), without that feature:
/// the feature also turns on wasmtime's on-disk cache, threads and GC, which
/// this one snapshot needs none of. Adapted from wasmtime-wizer 49.0.6
/// (`src/wasmtime.rs`, Apache-2.0 WITH LLVM-exception).
async fn snapshot(
    wizer: &wasmtime_wizer::Wizer,
    store: &mut Store<WasiP1Ctx>,
    linker: &Linker<WasiP1Ctx>,
    wasm: &[u8],
) -> wasmtime::Result<Vec<u8>> {
    let (cx, instrumented) = wizer.instrument(wasm)?;
    let module = Module::new(store.engine(), &instrumented)?;
    let mut linker = linker.clone();
    // The engine also imports component-model functions (wasi:io and the
    // App's own host imports) that the initializer never calls; Wizer forbids
    // calling imports other than WASI during initialization.
    linker.define_unknown_imports_as_traps(&module)?;
    let instance = linker.instantiate_async(&mut *store, &module).await?;
    if let Some(Extern::Func(func)) = instance.get_export(&mut *store, "_initialize") {
        func.typed::<(), ()>(&*store)?
            .call_async(&mut *store, ())
            .await?;
    }
    let init = wizer.get_init_func().to_string();
    if init != "_initialize" {
        instance
            .get_typed_func::<(), ()>(&mut *store, &init)
            .map_err(|e| wasmtime::format_err!("the module has no `{init}: [] -> []` export: {e}"))?
            .call_async(&mut *store, ())
            .await?;
    }
    wizer
        .snapshot(&cx, &mut Snapshot { store, instance })
        .await
        .map_err(|e| wasmtime::format_err!("{e}"))
}

struct Snapshot<'a> {
    store: &'a mut Store<WasiP1Ctx>,
    instance: Instance,
}

impl InstanceState for Snapshot<'_> {
    async fn global_get(&mut self, name: &str, _: ValType) -> SnapshotVal {
        let global = self
            .instance
            .get_global(&mut *self.store, name)
            .expect("instrumented module exports every global");
        match global.get(&mut *self.store) {
            Val::I32(x) => SnapshotVal::I32(x),
            Val::I64(x) => SnapshotVal::I64(x),
            Val::F32(x) => SnapshotVal::F32(x),
            Val::F64(x) => SnapshotVal::F64(x),
            Val::V128(x) => SnapshotVal::V128(x.as_u128()),
            _ => panic!("unsupported global value type"),
        }
    }

    async fn memory_contents(&mut self, name: &str, contents: impl FnOnce(&[u8]) + Send) {
        let memory = self
            .instance
            .get_memory(&mut *self.store, name)
            .expect("instrumented module exports every memory");
        contents(memory.data(&*self.store))
    }
}

/// The Wizer entry point: parse `args`, read stdin, run.
pub fn main(args: &[String]) -> Result<(), String> {
    let parsed = parse_args(args)?;
    let mut stdin = Vec::new();
    std::io::stdin()
        .read_to_end(&mut stdin)
        .map_err(|e| format!("reading stdin: {e}"))?;
    run(&parsed, stdin).map_err(|e| format!("{e:?}"))
}

/// Drive a future that never waits on I/O to completion. Nothing here is
/// truly asynchronous (wasmtime's async API is used only because the WASI
/// preview 1 linker requires it), so polling in a loop terminates.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_componentize_js_invocation() {
        let args = s(&[
            "--allow-wasi",
            "--init-func",
            "componentize.wizer",
            "--dir=/tmp/x/sources",
            "--mapdir=/::/tmp/stage",
            "--wasm-bulk-memory=true",
            "--inherit-env=true",
            "-o=/tmp/x/out.wasm",
            "/tmp/x/in.wasm",
        ]);
        assert!(is_wizer_invocation(&args));
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.init_func, "componentize.wizer");
        assert_eq!(parsed.dirs, vec!["/tmp/x/sources"]);
        assert_eq!(
            parsed.mapdirs,
            vec![("/".to_string(), "/tmp/stage".to_string())]
        );
        assert_eq!(parsed.output, "/tmp/x/out.wasm");
        assert_eq!(parsed.input, "/tmp/x/in.wasm");
    }

    #[test]
    fn refuses_unknown_options() {
        let args = s(&["--allow-wasi", "--keep-init-func=true", "-o=o", "i"]);
        assert!(parse_args(&args).is_err());
    }
}
