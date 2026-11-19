//! The WASI 0.2 subset an App gets (see `wit-wasi/world.wit`), so that
//! ordinary std Rust and ComponentizeJS guests load. None of it grants
//! anything the `pollis:app/host` grants do not:
//!
//! - environment and arguments are empty, there is no working directory;
//! - stdin is closed; stdout and stderr go to the runtime log at debug level,
//!   capped per job; nothing reaches the group;
//! - `exit` traps the job, so its queued actions are discarded;
//! - clocks and randomness come from the host (time from the runtime);
//! - the filesystem has no preopened directories, so no descriptor can ever
//!   exist and every descriptor method is unreachable;
//! - sockets and `wasi:http` are not linked, and [`check_import`] refuses a
//!   component that imports them.
//!
//! Every call counts toward the job's host-call cap and checks its deadline.

use std::time::{Duration, Instant};

use wasmtime::component::{Resource, ResourceTable};

use crate::state::{cap_hit, DeadlineHit, JobState};
use crate::types::LogLevel;

#[allow(clippy::all)]
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "wit-wasi",
        world: "pollis:host-wasi/app-wasi",
        imports: { default: trappable },
        with: {
            "wasi:io/error.error": super::IoError,
            "wasi:io/poll.pollable": super::Pollable,
            "wasi:io/streams.input-stream": super::InputStream,
            "wasi:io/streams.output-stream": super::OutputStream,
            "wasi:cli/terminal-input.terminal-input": super::Terminal,
            "wasi:cli/terminal-output.terminal-output": super::Terminal,
            "wasi:filesystem/types.descriptor": super::Descriptor,
            "wasi:filesystem/types.directory-entry-stream": super::DirectoryEntryStream,
        },
    });
}

use bindings::wasi::cli::{
    environment, exit, stderr, stdin, stdout, terminal_input, terminal_output,
};
use bindings::wasi::cli::{terminal_stderr, terminal_stdin, terminal_stdout};
use bindings::wasi::clocks::{monotonic_clock, wall_clock};
use bindings::wasi::filesystem::{preopens, types as fs};
use bindings::wasi::io::{error as io_error, poll, streams};
use bindings::wasi::random::{insecure, insecure_seed, random};

/// Total stdout + stderr bytes logged per job; the rest is dropped.
const MAX_STDIO_BYTES: usize = 64 << 10;
/// Most bytes one `get-random-bytes` call may ask for.
const MAX_RANDOM_BYTES: u64 = 1 << 20;

/// Interface prefixes an App may import, at any 0.2.x version.
const WASI_ALLOWED: [&str; 20] = [
    "wasi:cli/environment",
    "wasi:cli/exit",
    "wasi:cli/stdin",
    "wasi:cli/stdout",
    "wasi:cli/stderr",
    "wasi:cli/terminal-input",
    "wasi:cli/terminal-output",
    "wasi:cli/terminal-stdin",
    "wasi:cli/terminal-stdout",
    "wasi:cli/terminal-stderr",
    "wasi:io/error",
    "wasi:io/poll",
    "wasi:io/streams",
    "wasi:clocks/monotonic-clock",
    "wasi:clocks/wall-clock",
    "wasi:random/random",
    "wasi:random/insecure",
    "wasi:random/insecure-seed",
    "wasi:filesystem/types",
    "wasi:filesystem/preopens",
];

/// Whether a component import is one the host provides. The error names the
/// import and says what to use instead.
pub fn check_import(name: &str) -> Result<(), String> {
    if name == "pollis:app/types@0.1.0" || name == "pollis:app/host@0.1.0" {
        return Ok(());
    }
    let (base, version) = name.split_once('@').unwrap_or((name, ""));
    if WASI_ALLOWED.contains(&base) && version.starts_with("0.2.") {
        return Ok(());
    }
    if base.starts_with("wasi:sockets/") || base.starts_with("wasi:http/") {
        return Err(format!(
            "import `{name}` refused: Apps have no sockets or direct network access; use `http-fetch` with hosts listed in `egress`"
        ));
    }
    Err(format!(
        "import `{name}` refused: an App may import only `pollis:app/host@0.1.0` and the WASI 0.2 cli, io, clocks, random and (empty) filesystem interfaces"
    ))
}

pub struct IoError;
pub enum Pollable {
    Ready,
    Until(Instant),
}
pub struct InputStream;
pub struct OutputStream(OutputKind);
#[derive(Clone, Copy)]
pub enum OutputKind {
    Stdout,
    Stderr,
}
pub struct Terminal;
/// Uninhabited: with no preopens, no descriptor is ever created.
pub enum Descriptor {}
pub enum DirectoryEntryStream {}

/// The App called `wasi:cli/exit`.
#[derive(Debug)]
pub struct ExitCalled;

impl std::fmt::Display for ExitCalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the App called `exit`; return from the handler instead")
    }
}

impl std::error::Error for ExitCalled {}

pub struct WasiState {
    table: ResourceTable,
    started: Instant,
    stdio_bytes: usize,
    /// Partial lines, per stream, until a newline or the job ends.
    stdout_line: Vec<u8>,
    stderr_line: Vec<u8>,
    /// Set when the App called `exit`.
    pub exited: bool,
}

impl WasiState {
    pub fn new() -> WasiState {
        WasiState {
            table: ResourceTable::new(),
            started: Instant::now(),
            stdio_bytes: 0,
            stdout_line: Vec::new(),
            stderr_line: Vec::new(),
            exited: false,
        }
    }
}

/// Add the WASI subset to the App linker.
pub fn add_to_linker(linker: &mut wasmtime::component::Linker<JobState>) -> wasmtime::Result<()> {
    bindings::AppWasi::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
        linker,
        &bindings::LinkOptions::default(),
        |s: &mut JobState| s,
    )
}

fn no_fs() -> wasmtime::Error {
    wasmtime::Error::msg("no filesystem: an App has no preopened directories")
}

impl JobState {
    fn wasi_enter(&mut self, iface: &str) -> wasmtime::Result<()> {
        self.enter(iface)
    }

    /// Sleep until `until`, never past the job deadline.
    fn sleep_until(&self, until: Instant) -> wasmtime::Result<()> {
        if until >= self.deadline {
            std::thread::sleep(self.deadline.saturating_duration_since(Instant::now()));
            return Err(wasmtime::Error::new(DeadlineHit));
        }
        std::thread::sleep(until.saturating_duration_since(Instant::now()));
        Ok(())
    }

    fn stdio(&mut self, stream: &OutputKind, bytes: &[u8]) {
        let room = MAX_STDIO_BYTES.saturating_sub(self.wasi.stdio_bytes);
        let take = &bytes[..bytes.len().min(room)];
        self.wasi.stdio_bytes += take.len();
        let buf = match stream {
            OutputKind::Stdout => &mut self.wasi.stdout_line,
            OutputKind::Stderr => &mut self.wasi.stderr_line,
        };
        buf.extend_from_slice(take);
        let mut lines = Vec::new();
        while let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            lines.push(line);
        }
        for line in lines {
            self.log_stdio(stream, &line);
        }
    }

    fn log_stdio(&self, stream: &OutputKind, line: &[u8]) {
        let which = match stream {
            OutputKind::Stdout => "stdout",
            OutputKind::Stderr => "stderr",
        };
        let line = &line[..line.len().min(self.limits.max_log_bytes)];
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches(['\n', '\r']);
        if !text.is_empty() {
            self.services.log(
                &self.ctx.installation_id,
                LogLevel::Debug,
                &format!("{which}: {text}"),
            );
        }
    }

    /// Log any unterminated stdout/stderr line. Called when the job ends.
    pub(crate) fn flush_stdio(&mut self) {
        let out = std::mem::take(&mut self.wasi.stdout_line);
        let err = std::mem::take(&mut self.wasi.stderr_line);
        self.log_stdio(&OutputKind::Stdout, &out);
        self.log_stdio(&OutputKind::Stderr, &err);
    }

    /// `from + nanos`, saturating past the job deadline (a guest-chosen value
    /// must never panic the host).
    fn later(&self, from: Instant, nanos: u64) -> Instant {
        from.checked_add(Duration::from_nanos(nanos))
            .unwrap_or(self.deadline + Duration::from_secs(1))
    }

    fn pollable_ready(&self, p: &Pollable) -> bool {
        match p {
            Pollable::Ready => true,
            Pollable::Until(t) => Instant::now() >= *t,
        }
    }
}

impl environment::Host for JobState {
    fn get_environment(&mut self) -> wasmtime::Result<Vec<(String, String)>> {
        self.wasi_enter("wasi:cli/environment")?;
        Ok(Vec::new())
    }

    fn get_arguments(&mut self) -> wasmtime::Result<Vec<String>> {
        self.wasi_enter("wasi:cli/environment")?;
        Ok(Vec::new())
    }

    fn initial_cwd(&mut self) -> wasmtime::Result<Option<String>> {
        self.wasi_enter("wasi:cli/environment")?;
        Ok(None)
    }
}

impl exit::Host for JobState {
    fn exit(&mut self, _status: Result<(), ()>) -> wasmtime::Result<()> {
        self.wasi.exited = true;
        Err(wasmtime::Error::new(ExitCalled))
    }

    fn exit_with_code(&mut self, _status_code: u8) -> wasmtime::Result<()> {
        self.wasi.exited = true;
        Err(wasmtime::Error::new(ExitCalled))
    }
}

impl stdin::Host for JobState {
    fn get_stdin(&mut self) -> wasmtime::Result<Resource<InputStream>> {
        self.wasi_enter("wasi:cli/stdin")?;
        Ok(self.wasi.table.push(InputStream)?)
    }
}

impl stdout::Host for JobState {
    fn get_stdout(&mut self) -> wasmtime::Result<Resource<OutputStream>> {
        self.wasi_enter("wasi:cli/stdout")?;
        Ok(self.wasi.table.push(OutputStream(OutputKind::Stdout))?)
    }
}

impl stderr::Host for JobState {
    fn get_stderr(&mut self) -> wasmtime::Result<Resource<OutputStream>> {
        self.wasi_enter("wasi:cli/stderr")?;
        Ok(self.wasi.table.push(OutputStream(OutputKind::Stderr))?)
    }
}

impl terminal_input::HostTerminalInput for JobState {
    fn drop(&mut self, rep: Resource<Terminal>) -> wasmtime::Result<()> {
        self.wasi.table.delete(rep)?;
        Ok(())
    }
}

impl terminal_input::Host for JobState {}

impl terminal_output::HostTerminalOutput for JobState {
    fn drop(&mut self, rep: Resource<Terminal>) -> wasmtime::Result<()> {
        self.wasi.table.delete(rep)?;
        Ok(())
    }
}

impl terminal_output::Host for JobState {}

impl terminal_stdin::Host for JobState {
    fn get_terminal_stdin(&mut self) -> wasmtime::Result<Option<Resource<Terminal>>> {
        Ok(None)
    }
}

impl terminal_stdout::Host for JobState {
    fn get_terminal_stdout(&mut self) -> wasmtime::Result<Option<Resource<Terminal>>> {
        Ok(None)
    }
}

impl terminal_stderr::Host for JobState {
    fn get_terminal_stderr(&mut self) -> wasmtime::Result<Option<Resource<Terminal>>> {
        Ok(None)
    }
}

impl io_error::HostError for JobState {
    fn to_debug_string(&mut self, _self_: Resource<IoError>) -> wasmtime::Result<String> {
        Ok(String::new())
    }

    fn drop(&mut self, rep: Resource<IoError>) -> wasmtime::Result<()> {
        self.wasi.table.delete(rep)?;
        Ok(())
    }
}

impl io_error::Host for JobState {}

impl poll::HostPollable for JobState {
    fn ready(&mut self, self_: Resource<Pollable>) -> wasmtime::Result<bool> {
        self.wasi_enter("wasi:io/poll")?;
        let p = self.wasi.table.get(&self_)?;
        Ok(self.pollable_ready(p))
    }

    fn block(&mut self, self_: Resource<Pollable>) -> wasmtime::Result<()> {
        self.wasi_enter("wasi:io/poll")?;
        match self.wasi.table.get(&self_)? {
            Pollable::Ready => Ok(()),
            Pollable::Until(t) => {
                let t = *t;
                self.sleep_until(t)
            }
        }
    }

    fn drop(&mut self, rep: Resource<Pollable>) -> wasmtime::Result<()> {
        self.wasi.table.delete(rep)?;
        Ok(())
    }
}

impl poll::Host for JobState {
    fn poll(&mut self, in_: Vec<Resource<Pollable>>) -> wasmtime::Result<Vec<u32>> {
        self.wasi_enter("wasi:io/poll")?;
        if in_.is_empty() {
            return Err(wasmtime::Error::msg("`poll` called with no pollables"));
        }
        loop {
            let mut ready = Vec::new();
            let mut earliest: Option<Instant> = None;
            for (i, r) in in_.iter().enumerate() {
                let p = self.wasi.table.get(r)?;
                if self.pollable_ready(p) {
                    ready.push(i as u32);
                } else if let Pollable::Until(t) = p {
                    earliest = Some(earliest.map_or(*t, |e| e.min(*t)));
                }
            }
            if !ready.is_empty() {
                return Ok(ready);
            }
            match earliest {
                Some(t) => self.sleep_until(t)?,
                None => return Ok(ready),
            }
        }
    }
}

impl streams::HostInputStream for JobState {
    fn read(
        &mut self,
        _s: Resource<InputStream>,
        _len: u64,
    ) -> wasmtime::Result<Result<Vec<u8>, streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Err(streams::StreamError::Closed))
    }

    fn blocking_read(
        &mut self,
        _s: Resource<InputStream>,
        _len: u64,
    ) -> wasmtime::Result<Result<Vec<u8>, streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Err(streams::StreamError::Closed))
    }

    fn skip(
        &mut self,
        _s: Resource<InputStream>,
        _len: u64,
    ) -> wasmtime::Result<Result<u64, streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Err(streams::StreamError::Closed))
    }

    fn blocking_skip(
        &mut self,
        _s: Resource<InputStream>,
        _len: u64,
    ) -> wasmtime::Result<Result<u64, streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Err(streams::StreamError::Closed))
    }

    fn subscribe(&mut self, _s: Resource<InputStream>) -> wasmtime::Result<Resource<Pollable>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(self.wasi.table.push(Pollable::Ready)?)
    }

    fn drop(&mut self, rep: Resource<InputStream>) -> wasmtime::Result<()> {
        self.wasi.table.delete(rep)?;
        Ok(())
    }
}

impl JobState {
    fn write_out(
        &mut self,
        s: &Resource<OutputStream>,
        contents: &[u8],
    ) -> wasmtime::Result<Result<(), streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        let kind = self.wasi.table.get(s)?.0;
        self.stdio(&kind, contents);
        Ok(Ok(()))
    }
}

impl streams::HostOutputStream for JobState {
    fn check_write(
        &mut self,
        _s: Resource<OutputStream>,
    ) -> wasmtime::Result<Result<u64, streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Ok(4096))
    }

    fn write(
        &mut self,
        s: Resource<OutputStream>,
        contents: Vec<u8>,
    ) -> wasmtime::Result<Result<(), streams::StreamError>> {
        self.write_out(&s, &contents)
    }

    fn blocking_write_and_flush(
        &mut self,
        s: Resource<OutputStream>,
        contents: Vec<u8>,
    ) -> wasmtime::Result<Result<(), streams::StreamError>> {
        self.write_out(&s, &contents)
    }

    fn flush(
        &mut self,
        _s: Resource<OutputStream>,
    ) -> wasmtime::Result<Result<(), streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Ok(()))
    }

    fn blocking_flush(
        &mut self,
        _s: Resource<OutputStream>,
    ) -> wasmtime::Result<Result<(), streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Ok(()))
    }

    fn subscribe(&mut self, _s: Resource<OutputStream>) -> wasmtime::Result<Resource<Pollable>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(self.wasi.table.push(Pollable::Ready)?)
    }

    fn write_zeroes(
        &mut self,
        _s: Resource<OutputStream>,
        _len: u64,
    ) -> wasmtime::Result<Result<(), streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Ok(()))
    }

    fn blocking_write_zeroes_and_flush(
        &mut self,
        _s: Resource<OutputStream>,
        _len: u64,
    ) -> wasmtime::Result<Result<(), streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Ok(()))
    }

    fn splice(
        &mut self,
        _s: Resource<OutputStream>,
        _src: Resource<InputStream>,
        _len: u64,
    ) -> wasmtime::Result<Result<u64, streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Err(streams::StreamError::Closed))
    }

    fn blocking_splice(
        &mut self,
        _s: Resource<OutputStream>,
        _src: Resource<InputStream>,
        _len: u64,
    ) -> wasmtime::Result<Result<u64, streams::StreamError>> {
        self.wasi_enter("wasi:io/streams")?;
        Ok(Err(streams::StreamError::Closed))
    }

    fn drop(&mut self, rep: Resource<OutputStream>) -> wasmtime::Result<()> {
        self.wasi.table.delete(rep)?;
        Ok(())
    }
}

impl streams::Host for JobState {}

impl monotonic_clock::Host for JobState {
    fn now(&mut self) -> wasmtime::Result<u64> {
        self.wasi_enter("wasi:clocks/monotonic-clock")?;
        Ok(self.wasi.started.elapsed().as_nanos() as u64)
    }

    fn resolution(&mut self) -> wasmtime::Result<u64> {
        self.wasi_enter("wasi:clocks/monotonic-clock")?;
        Ok(1)
    }

    fn subscribe_instant(&mut self, when: u64) -> wasmtime::Result<Resource<Pollable>> {
        self.wasi_enter("wasi:clocks/monotonic-clock")?;
        let at = self.later(self.wasi.started, when);
        Ok(self.wasi.table.push(Pollable::Until(at))?)
    }

    fn subscribe_duration(&mut self, when: u64) -> wasmtime::Result<Resource<Pollable>> {
        self.wasi_enter("wasi:clocks/monotonic-clock")?;
        let at = self.later(Instant::now(), when);
        Ok(self.wasi.table.push(Pollable::Until(at))?)
    }
}

impl wall_clock::Host for JobState {
    fn now(&mut self) -> wasmtime::Result<wall_clock::Datetime> {
        self.wasi_enter("wasi:clocks/wall-clock")?;
        let ms = self.services.now_ms();
        Ok(wall_clock::Datetime {
            seconds: ms / 1000,
            nanoseconds: ((ms % 1000) * 1_000_000) as u32,
        })
    }

    fn resolution(&mut self) -> wasmtime::Result<wall_clock::Datetime> {
        self.wasi_enter("wasi:clocks/wall-clock")?;
        Ok(wall_clock::Datetime {
            seconds: 0,
            nanoseconds: 1_000_000,
        })
    }
}

fn random_bytes(len: u64) -> wasmtime::Result<Vec<u8>> {
    if len > MAX_RANDOM_BYTES {
        return Err(cap_hit(
            "get-random-bytes",
            "random bytes per call",
            MAX_RANDOM_BYTES as u32,
        ));
    }
    let mut out = vec![0u8; len as usize];
    getrandom::fill(&mut out).map_err(|e| wasmtime::Error::msg(format!("random: {e}")))?;
    Ok(out)
}

fn random_u64() -> wasmtime::Result<u64> {
    let bytes = random_bytes(8)?;
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes);
    Ok(u64::from_le_bytes(b))
}

impl random::Host for JobState {
    fn get_random_bytes(&mut self, len: u64) -> wasmtime::Result<Vec<u8>> {
        self.wasi_enter("wasi:random/random")?;
        random_bytes(len)
    }

    fn get_random_u64(&mut self) -> wasmtime::Result<u64> {
        self.wasi_enter("wasi:random/random")?;
        random_u64()
    }
}

impl insecure::Host for JobState {
    fn get_insecure_random_bytes(&mut self, len: u64) -> wasmtime::Result<Vec<u8>> {
        self.wasi_enter("wasi:random/insecure")?;
        random_bytes(len)
    }

    fn get_insecure_random_u64(&mut self) -> wasmtime::Result<u64> {
        self.wasi_enter("wasi:random/insecure")?;
        random_u64()
    }
}

impl insecure_seed::Host for JobState {
    fn insecure_seed(&mut self) -> wasmtime::Result<(u64, u64)> {
        self.wasi_enter("wasi:random/insecure-seed")?;
        Ok((random_u64()?, random_u64()?))
    }
}

impl preopens::Host for JobState {
    fn get_directories(&mut self) -> wasmtime::Result<Vec<(Resource<Descriptor>, String)>> {
        self.wasi_enter("wasi:filesystem/preopens")?;
        Ok(Vec::new())
    }
}

impl fs::Host for JobState {
    fn filesystem_error_code(
        &mut self,
        _err: Resource<IoError>,
    ) -> wasmtime::Result<Option<fs::ErrorCode>> {
        Ok(None)
    }
}

impl fs::HostDirectoryEntryStream for JobState {
    fn read_directory_entry(
        &mut self,
        _self_: Resource<DirectoryEntryStream>,
    ) -> wasmtime::Result<Result<Option<fs::DirectoryEntry>, fs::ErrorCode>> {
        Err(no_fs())
    }

    fn drop(&mut self, _rep: Resource<DirectoryEntryStream>) -> wasmtime::Result<()> {
        Err(no_fs())
    }
}

type D = Resource<Descriptor>;
type R<T> = wasmtime::Result<Result<T, fs::ErrorCode>>;

impl fs::HostDescriptor for JobState {
    fn read_via_stream(&mut self, _: D, _: fs::Filesize) -> R<Resource<InputStream>> {
        Err(no_fs())
    }
    fn write_via_stream(&mut self, _: D, _: fs::Filesize) -> R<Resource<OutputStream>> {
        Err(no_fs())
    }
    fn append_via_stream(&mut self, _: D) -> R<Resource<OutputStream>> {
        Err(no_fs())
    }
    fn advise(&mut self, _: D, _: fs::Filesize, _: fs::Filesize, _: fs::Advice) -> R<()> {
        Err(no_fs())
    }
    fn sync_data(&mut self, _: D) -> R<()> {
        Err(no_fs())
    }
    fn get_flags(&mut self, _: D) -> R<fs::DescriptorFlags> {
        Err(no_fs())
    }
    fn get_type(&mut self, _: D) -> R<fs::DescriptorType> {
        Err(no_fs())
    }
    fn set_size(&mut self, _: D, _: fs::Filesize) -> R<()> {
        Err(no_fs())
    }
    fn set_times(&mut self, _: D, _: fs::NewTimestamp, _: fs::NewTimestamp) -> R<()> {
        Err(no_fs())
    }
    fn read(&mut self, _: D, _: fs::Filesize, _: fs::Filesize) -> R<(Vec<u8>, bool)> {
        Err(no_fs())
    }
    fn write(&mut self, _: D, _: Vec<u8>, _: fs::Filesize) -> R<fs::Filesize> {
        Err(no_fs())
    }
    fn read_directory(&mut self, _: D) -> R<Resource<DirectoryEntryStream>> {
        Err(no_fs())
    }
    fn sync(&mut self, _: D) -> R<()> {
        Err(no_fs())
    }
    fn create_directory_at(&mut self, _: D, _: String) -> R<()> {
        Err(no_fs())
    }
    fn stat(&mut self, _: D) -> R<fs::DescriptorStat> {
        Err(no_fs())
    }
    fn stat_at(&mut self, _: D, _: fs::PathFlags, _: String) -> R<fs::DescriptorStat> {
        Err(no_fs())
    }
    fn set_times_at(
        &mut self,
        _: D,
        _: fs::PathFlags,
        _: String,
        _: fs::NewTimestamp,
        _: fs::NewTimestamp,
    ) -> R<()> {
        Err(no_fs())
    }
    fn link_at(&mut self, _: D, _: fs::PathFlags, _: String, _: D, _: String) -> R<()> {
        Err(no_fs())
    }
    fn open_at(
        &mut self,
        _: D,
        _: fs::PathFlags,
        _: String,
        _: fs::OpenFlags,
        _: fs::DescriptorFlags,
    ) -> R<D> {
        Err(no_fs())
    }
    fn readlink_at(&mut self, _: D, _: String) -> R<String> {
        Err(no_fs())
    }
    fn remove_directory_at(&mut self, _: D, _: String) -> R<()> {
        Err(no_fs())
    }
    fn rename_at(&mut self, _: D, _: String, _: D, _: String) -> R<()> {
        Err(no_fs())
    }
    fn symlink_at(&mut self, _: D, _: String, _: String) -> R<()> {
        Err(no_fs())
    }
    fn unlink_file_at(&mut self, _: D, _: String) -> R<()> {
        Err(no_fs())
    }
    fn is_same_object(&mut self, _: D, _: D) -> wasmtime::Result<bool> {
        Err(no_fs())
    }
    fn metadata_hash(&mut self, _: D) -> R<fs::MetadataHashValue> {
        Err(no_fs())
    }
    fn metadata_hash_at(&mut self, _: D, _: fs::PathFlags, _: String) -> R<fs::MetadataHashValue> {
        Err(no_fs())
    }
    fn drop(&mut self, _rep: D) -> wasmtime::Result<()> {
        Err(no_fs())
    }
}
