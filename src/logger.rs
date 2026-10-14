//! The `Logger`, its builder, the background writer thread, the global logger
//! and the `log_*!` macros.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use crate::entry::LogEntry;
use crate::filter::TargetFilter;
use crate::level::Level;
use crate::sink::{FileSink, Sink, StderrSink, StdoutSink};
use crate::syslog::{default_ident, Facility, RemoteSyslogSink, SyslogSink};
use crate::time::Timestamp;

/// Default capacity of the internal bounded queue.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 8192;
/// Default maximum size of a rotated log file (10 MiB).
pub const DEFAULT_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
/// Number of consecutive write failures tolerated before falling back to
/// standard error.
const MAX_CONSECUTIVE_WRITE_ERRORS: usize = 8;

/// Strategy applied when the internal bounded queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backpressure {
    /// Block the calling thread until the writer catches up. No records are
    /// ever dropped, at the cost of potentially slowing down callers.
    Block,
    /// Drop the newest record and count it in
    /// [`Logger::dropped_message_count`]. Callers never block.
    DropNewest,
}

/// Where the logger writes its output.
enum SinkSource {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
    /// A size-rotating file at `path`, with `max_size` and `max_backups`.
    File(PathBuf, u64, usize),
    /// The local syslog daemon.
    Syslog(SyslogSink),
    /// A remote syslog server over UDP.
    RemoteSyslog(RemoteSyslogSink),
    /// A caller-provided sink.
    Custom(Box<dyn Sink>),
}

/// A message sent from callers to the writer thread.
enum Message {
    /// A formatted log record.
    Log(LogEntry),
    /// Ask the writer to flush and acknowledge once done.
    Flush(Sender<()>),
    /// Ask the writer to flush, acknowledge and exit.
    Shutdown(Sender<()>),
}

/// Shared state between all [`Logger`] clones.
struct Inner {
    tx: Mutex<SyncSender<Message>>,
    level: Level,
    filter: Arc<TargetFilter>,
    time_offset_seconds: i32,
    backpressure: Backpressure,
    handle: Mutex<Option<JoinHandle<()>>>,
    closed: AtomicBool,
    dropped: AtomicUsize,
    json: bool,
}

/// A handle to a running logger.
///
/// [`Logger`] is cheaply cloneable and may be shared freely across threads. All
/// clones feed the same writer thread. The logger is fully drained when the
/// last clone is dropped; call [`Logger::shutdown`] for deterministic teardown.
#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}

impl Logger {
    /// Whether records at `level` will be recorded for the default target.
    ///
    /// "Default target" means a target matched by no per-target directive, so
    /// this reflects the global default directive or the logger's base level.
    /// For a target-aware check use [`Logger::is_enabled_for`].
    #[must_use]
    pub fn is_enabled(&self, level: Level) -> bool {
        self.inner.filter.enabled("", level, self.inner.level)
    }

    /// Whether records at `level` from module `target` will be recorded.
    ///
    /// `target` is the module path (e.g. `my_app::server`) attached to the
    /// record. Per-target directives from the [`TargetFilter`] apply here, with
    /// unmatched targets falling back to the logger's base level.
    #[must_use]
    pub fn is_enabled_for(&self, level: Level, target: &str) -> bool {
        self.inner.filter.enabled(target, level, self.inner.level)
    }

    /// The threshold level of this logger.
    ///
    /// This is the base level configured on the builder; per-target directives
    /// may raise or lower the effective level for individual modules.
    #[must_use]
    pub fn level(&self) -> Level {
        self.inner.level
    }

    /// The per-target filter applied to this logger.
    ///
    /// Returns an empty filter when no directive is active, in which case the
    /// base level applies to every target.
    #[must_use]
    pub fn filter(&self) -> &TargetFilter {
        &self.inner.filter
    }

    /// Enqueue a log record.
    ///
    /// The record's timestamp is replaced with the current wall-clock time
    /// (using this logger's configured offset). Records below the logger's
    /// effective threshold for the record's target are discarded. The calling
    /// thread's current context (see [`crate::push_context`]) is merged into
    /// the record's fields. If the writer thread has died, the record is
    /// written synchronously to standard error as a last resort.
    pub fn log(&self, mut entry: LogEntry) {
        if self.inner.closed.load(Ordering::Acquire) {
            return;
        }
        if !self.is_enabled_for(entry.level, entry.target) {
            return;
        }
        entry.timestamp = Timestamp::now(self.inner.time_offset_seconds);
        entry = entry.merge_context(crate::context::current_context());

        let tx = self.tx_guard();
        let msg = Message::Log(entry);

        let send_result = match self.inner.backpressure {
            Backpressure::Block => tx.send(msg),
            Backpressure::DropNewest => match tx.try_send(msg) {
                Ok(()) => return,
                Err(TrySendError::Full(queued)) => {
                    self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                    drop(queued);
                    return;
                }
                Err(TrySendError::Disconnected(queued)) => Err(mpsc::SendError(queued)),
            },
        };

        if let Err(mpsc::SendError(Message::Log(entry))) = send_result {
            let _ = emergency_write(&entry, self.inner.json);
        }
    }

    /// Convenience: log a message at the [`Level::Trace`] level.
    pub fn trace(&self, message: impl Into<String>) {
        self.log(LogEntry::new(Level::Trace, message.into(), "", "", 0));
    }

    /// Convenience: log a message at the [`Level::Debug`] level.
    pub fn debug(&self, message: impl Into<String>) {
        self.log(LogEntry::new(Level::Debug, message.into(), "", "", 0));
    }

    /// Convenience: log a message at the [`Level::Info`] level.
    pub fn info(&self, message: impl Into<String>) {
        self.log(LogEntry::new(Level::Info, message.into(), "", "", 0));
    }

    /// Convenience: log a message at the [`Level::Warn`] level.
    pub fn warn(&self, message: impl Into<String>) {
        self.log(LogEntry::new(Level::Warn, message.into(), "", "", 0));
    }

    /// Convenience: log a message at the [`Level::Error`] level.
    pub fn error(&self, message: impl Into<String>) {
        self.log(LogEntry::new(Level::Error, message.into(), "", "", 0));
    }

    /// Block until every record enqueued so far has been written to the sink.
    pub fn flush(&self) {
        let (ack, rx) = mpsc::channel();
        if self.send_ctrl(Message::Flush(ack)) {
            let _ = rx.recv();
        }
    }

    /// Flush all pending records, stop the writer thread and join it.
    ///
    /// After this call the logger is closed: further calls to
    /// [`Logger::log`] are no-ops. The consumed handle is invalid; other
    /// clones still point at the (now stopped) logger.
    pub fn shutdown(self) {
        self.inner.closed.store(true, Ordering::Release);
        let (ack, rx) = mpsc::channel();
        if self.send_ctrl(Message::Shutdown(ack)) {
            let _ = rx.recv();
            if let Ok(mut handle) = self.inner.handle.lock() {
                if let Some(join) = handle.take() {
                    let _ = join.join();
                }
            }
        }
    }

    /// Number of records dropped because the queue was full (only counted
    /// with [`Backpressure::DropNewest`]).
    #[must_use]
    pub fn dropped_message_count(&self) -> usize {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Send a control message to the writer thread.
    fn send_ctrl(&self, msg: Message) -> bool {
        self.tx_guard().send(msg).is_ok()
    }

    /// Lock the sender, surviving poisoning.
    fn tx_guard(&self) -> std::sync::MutexGuard<'_, SyncSender<Message>> {
        self.inner.tx.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Builder for [`Logger`].
///
/// # Example
///
/// ```no_run
/// use xoslog::{Level, LoggerBuilder};
///
/// let logger = LoggerBuilder::new()
///     .level(Level::Debug)
///     .backpressure(xoslog::Backpressure::Block)
///     .to_file("/tmp/app.log", 1024 * 1024, 5)
///     .unwrap()
///     .build()
///     .unwrap();
/// ```
pub struct LoggerBuilder {
    level: Level,
    capacity: usize,
    backpressure: Backpressure,
    include_location: bool,
    time_offset_seconds: i32,
    json: bool,
    filter: Option<TargetFilter>,
    read_env: bool,
    sinks: Vec<(SinkSource, Level)>,
}

impl Default for LoggerBuilder {
    fn default() -> Self {
        LoggerBuilder {
            level: Level::Info,
            capacity: DEFAULT_CHANNEL_CAPACITY,
            backpressure: Backpressure::Block,
            include_location: true,
            time_offset_seconds: 0,
            json: false,
            filter: None,
            read_env: true,
            sinks: Vec::new(),
        }
    }
}

impl LoggerBuilder {
    /// Create a builder with defaults (stdout sink, `Info` threshold).
    #[must_use]
    pub fn new() -> LoggerBuilder {
        LoggerBuilder::default()
    }

    /// Set the minimum level that will be recorded.
    #[must_use]
    pub fn level(mut self, level: Level) -> LoggerBuilder {
        self.level = level;
        self
    }

    /// Set the capacity of the internal bounded queue.
    #[must_use]
    pub fn channel_capacity(mut self, capacity: usize) -> LoggerBuilder {
        self.capacity = capacity;
        self
    }

    /// Set the policy used when the queue is full.
    #[must_use]
    pub fn backpressure(mut self, backpressure: Backpressure) -> LoggerBuilder {
        self.backpressure = backpressure;
        self
    }

    /// Whether to append the source location (`file:line @ module`) to each
    /// record. Enabled by default.
    #[must_use]
    pub fn include_location(mut self, include: bool) -> LoggerBuilder {
        self.include_location = include;
        self
    }

    /// Emit each record as a single flat JSON object per line instead of the
    /// plain-text format.
    ///
    /// The line looks like
    /// `{"ts":"...","level":"INFO","msg":"...","file":"...","line":N,"target":"...",<fields>}`,
    /// which aggregators such as Loki, ELK and Datadog ingest without a
    /// parser. Disabled by default.
    #[must_use]
    pub fn json(mut self) -> LoggerBuilder {
        self.json = true;
        self
    }

    /// Apply a per-target filter to the logger.
    ///
    /// When set, the [`XOSLOG`](crate::DEFAULT_FILTER_ENV) environment variable
    /// is ignored. See [`TargetFilter`] for the directive grammar.
    #[must_use]
    pub fn filter(mut self, filter: TargetFilter) -> LoggerBuilder {
        self.filter = Some(filter);
        self
    }

    /// Read the per-target filter from the
    /// [`XOSLOG`](crate::DEFAULT_FILTER_ENV) environment variable at build
    /// time.
    ///
    /// This is the default behaviour; the method exists so a filter can be
    /// re-enabled after [`LoggerBuilder::ignore_env_filter`].
    #[must_use]
    pub fn env_filter(mut self) -> LoggerBuilder {
        self.read_env = true;
        self
    }

    /// Do not read the [`XOSLOG`](crate::DEFAULT_FILTER_ENV) environment
    /// variable at build time.
    ///
    /// Useful when `XOSLOG` may be set in the calling environment but must not
    /// influence this logger. Any filter supplied via
    /// [`LoggerBuilder::filter`] is still applied.
    #[must_use]
    pub fn ignore_env_filter(mut self) -> LoggerBuilder {
        self.read_env = false;
        self
    }

    /// Shift timestamps by `seconds` relative to UTC (e.g. `19800` for
    /// UTC+05:30). Defaults to `0` (UTC).
    #[must_use]
    pub fn time_offset_seconds(mut self, seconds: i32) -> LoggerBuilder {
        self.time_offset_seconds = seconds;
        self
    }

    /// Write to standard output.
    #[must_use]
    pub fn to_stdout(mut self) -> LoggerBuilder {
        self.set_single_sink(SinkSource::Stdout);
        self
    }

    /// Write to standard error.
    #[must_use]
    pub fn to_stderr(mut self) -> LoggerBuilder {
        self.set_single_sink(SinkSource::Stderr);
        self
    }

    /// Write to a size-rotating file.
    ///
    /// `max_size` is the rotation threshold in bytes (`0` disables rotation);
    /// `max_backups` is how many numbered backups are kept (`0` truncates in
    /// place).
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the log file cannot be opened.
    pub fn to_file(
        mut self,
        path: impl AsRef<std::path::Path>,
        max_size: u64,
        max_backups: usize,
    ) -> std::io::Result<LoggerBuilder> {
        FileSink::open(&path, max_size, max_backups)?;
        self.set_single_sink(SinkSource::File(
            path.as_ref().to_path_buf(),
            max_size,
            max_backups,
        ));
        Ok(self)
    }

    /// Use a caller-provided sink.
    #[must_use]
    pub fn sink(mut self, sink: impl Sink + 'static) -> LoggerBuilder {
        self.set_single_sink(SinkSource::Custom(Box::new(sink)));
        self
    }

    /// Add an additional sink, fanning records out to it alongside every other
    /// sink configured on this builder.
    ///
    /// `min_level` is an absolute floor applied on top of the logger's own
    /// level and [`TargetFilter`]: the sink receives a record only when the
    /// record's level is at least `min_level`. Pass [`Level::Trace`] for no
    /// extra filtering, or [`Level::Off`] to disable the sink.
    ///
    /// Unlike [`LoggerBuilder::to_stdout`] and its siblings, which *replace* the
    /// configured sink, this method appends. A builder with no sink at all (the
    /// default) writes to standard output.
    ///
    /// Each sink is independent: if one fails repeatedly the writer diverts
    /// only that sink's records to standard error, leaving the others intact.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use xoslog::{FileSink, Level, LoggerBuilder};
    ///
    /// let file = FileSink::open("/var/log/app.log", 10 * 1024 * 1024, 3).unwrap();
    /// let logger = LoggerBuilder::new()
    ///     .to_stdout()                 // everything on the console
    ///     .add_sink(file, Level::Warn) // warnings and errors only, to disk
    ///     .build()
    ///     .unwrap();
    /// ```
    #[must_use]
    pub fn add_sink(mut self, sink: impl Sink + 'static, min_level: Level) -> LoggerBuilder {
        self.sinks
            .push((SinkSource::Custom(Box::new(sink)), min_level));
        self
    }

    /// Replace every configured sink with a single one at the most verbose
    /// level. Used by the `to_*` convenience methods.
    fn set_single_sink(&mut self, source: SinkSource) {
        self.sinks.clear();
        self.sinks.push((source, Level::Trace));
    }

    /// Write to the local syslog daemon over the standard Unix datagram
    /// sockets ([`crate::DEFAULT_SYSLOG_SOCKETS`]).
    ///
    /// The program identity (syslog tag) is derived from `argv[0]`. The
    /// connection is established lazily, so building the logger succeeds even
    /// if the daemon is temporarily unavailable; the writer reconnects
    /// automatically on every message.
    #[must_use]
    pub fn to_syslog(mut self, facility: Facility) -> LoggerBuilder {
        self.set_single_sink(SinkSource::Syslog(SyslogSink::local(
            facility,
            default_ident(),
        )));
        self
    }

    /// Write to a remote syslog server over UDP.
    ///
    /// `host` is resolved to the first address returned by
    /// `(host, port).to_socket_addrs()` (typically port 514 for syslog). The
    /// program identity (syslog tag) is derived from `argv[0]`.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if `host` cannot be resolved or the UDP
    /// socket cannot be created.
    pub fn to_remote_syslog(
        mut self,
        host: &str,
        port: u16,
        facility: Facility,
    ) -> std::io::Result<LoggerBuilder> {
        use std::net::ToSocketAddrs;
        let addr = (host, port).to_socket_addrs()?.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "hostname resolved to no addresses",
            )
        })?;
        self.set_single_sink(SinkSource::RemoteSyslog(RemoteSyslogSink::new(
            addr,
            facility,
            default_ident(),
        )?));
        Ok(self)
    }

    /// Build the logger: open the sink and spawn the writer thread.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the sink cannot be opened or the
    /// writer thread cannot be spawned.
    pub fn build(self) -> std::io::Result<Logger> {
        let mut slots: Vec<SinkSlot> = Vec::new();
        // A builder with no explicit sink defaults to standard output, matching
        // the historical default.
        if self.sinks.is_empty() {
            slots.push(SinkSlot::new(Box::new(StdoutSink), Level::Trace));
        }
        for (source, min_level) in self.sinks {
            let sink: Box<dyn Sink> = match source {
                SinkSource::Stdout => Box::new(StdoutSink),
                SinkSource::Stderr => Box::new(StderrSink),
                SinkSource::File(path, max_size, max_backups) => {
                    Box::new(FileSink::open(path, max_size, max_backups)?)
                }
                SinkSource::Syslog(sink) => Box::new(sink),
                SinkSource::RemoteSyslog(sink) => Box::new(sink),
                SinkSource::Custom(sink) => sink,
            };
            slots.push(SinkSlot::new(sink, min_level));
        }

        let (tx, rx) = mpsc::sync_channel(self.capacity);
        let include_location = self.include_location;
        let json = self.json;
        let handle = std::thread::Builder::new()
            .name("xoslog-writer".to_string())
            .spawn(move || writer_loop(rx, slots, include_location, json))?;

        let filter = Arc::new(match self.filter {
            Some(filter) => filter,
            None if self.read_env => TargetFilter::from_env().unwrap_or_default(),
            None => TargetFilter::default(),
        });

        let inner = Arc::new(Inner {
            tx: Mutex::new(tx),
            level: self.level,
            filter,
            time_offset_seconds: self.time_offset_seconds,
            backpressure: self.backpressure,
            handle: Mutex::new(Some(handle)),
            closed: AtomicBool::new(false),
            dropped: AtomicUsize::new(0),
            json,
        });

        Ok(Logger { inner })
    }
}

/// A configured sink together with its per-sink level floor and failure state.
struct SinkSlot {
    /// The destination the writer thread feeds.
    sink: Box<dyn Sink>,
    /// Records below this level are not delivered to this sink.
    min_level: Level,
    /// Consecutive write failures; reset to zero on the next success.
    consecutive_errors: usize,
    /// Set once the sink has failed too many times in a row; the writer then
    /// diverts this sink's records to standard error.
    degraded: bool,
}

impl SinkSlot {
    /// Wrap a sink with a level floor and fresh failure state.
    fn new(sink: Box<dyn Sink>, min_level: Level) -> SinkSlot {
        SinkSlot {
            sink,
            min_level,
            consecutive_errors: 0,
            degraded: false,
        }
    }
}

/// The writer thread: drains the queue, formats each record once and fans it
/// out to every configured sink.
fn writer_loop(
    rx: Receiver<Message>,
    mut slots: Vec<SinkSlot>,
    include_location: bool,
    json: bool,
) {
    let mut running = true;

    while running {
        let msg = match rx.recv() {
            Ok(msg) => msg,
            Err(_) => break,
        };

        let stop = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_message(msg, &mut slots, include_location, json)
        })) {
            Ok(stop) => stop,
            Err(_) => {
                // A panic escaped message handling (e.g. while formatting).
                // Degrade every sink so later records still reach stderr.
                for slot in &mut slots {
                    slot.degraded = true;
                }
                false
            }
        };

        running = !stop;
    }

    for slot in &mut slots {
        let _ = slot.sink.flush();
    }
}

/// Handle one message, writing a record to every eligible sink.
///
/// Returns `true` when the writer should stop.
fn handle_message(
    msg: Message,
    slots: &mut [SinkSlot],
    include_location: bool,
    json: bool,
) -> bool {
    match msg {
        Message::Log(entry) => {
            let mut buf = Vec::with_capacity(96 + entry.message.len());
            if json {
                crate::json::write_record(&entry, include_location, &mut buf);
            } else {
                format_record(&entry, include_location, &mut buf);
            }

            for slot in slots.iter_mut() {
                if entry.level < slot.min_level {
                    continue;
                }
                if slot.degraded {
                    write_stderr(&buf);
                    continue;
                }
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    slot.sink.write_entry(&entry, &buf)
                }));
                match outcome {
                    Ok(Ok(())) => slot.consecutive_errors = 0,
                    _ => {
                        slot.consecutive_errors += 1;
                        if slot.consecutive_errors >= MAX_CONSECUTIVE_WRITE_ERRORS {
                            slot.degraded = true;
                            write_stderr(b"[xoslog] a sink failed, falling back to stderr\n");
                        }
                    }
                }
            }
            false
        }
        Message::Flush(ack) => {
            for slot in slots.iter_mut() {
                let _ = slot.sink.flush();
            }
            let _ = ack.send(());
            false
        }
        Message::Shutdown(ack) => {
            for slot in slots.iter_mut() {
                let _ = slot.sink.flush();
            }
            let _ = ack.send(());
            true
        }
    }
}

/// Write raw bytes to standard error, ignoring errors.
fn write_stderr(bytes: &[u8]) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(bytes);
    let _ = err.flush();
}

/// Format a record into `out`, appending a trailing newline.
fn format_record(entry: &LogEntry, include_location: bool, out: &mut Vec<u8>) {
    use std::fmt::Write as _;
    let mut line = String::with_capacity(96 + entry.message.len());
    let _ = write!(
        line,
        "{} [{}] {}",
        entry.timestamp,
        entry.level.as_str(),
        entry.message
    );
    if include_location {
        let location = entry.location();
        if !location.is_empty() {
            let _ = write!(line, " ({location})");
        }
    }
    out.extend_from_slice(line.as_bytes());
    out.push(b'\n');
}

/// Synchronously write a record to standard error.
fn emergency_write(entry: &LogEntry, json: bool) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(96 + entry.message.len());
    if json {
        crate::json::write_record(entry, false, &mut buf);
    } else {
        format_record(entry, false, &mut buf);
    }
    let mut err = std::io::stderr().lock();
    err.write_all(b"[xoslog] writer thread unavailable, logging synchronously\n")?;
    err.write_all(&buf)?;
    err.flush()
}

// ---------------------------------------------------------------------------
// Global logger and macros
// ---------------------------------------------------------------------------

static GLOBAL: OnceLock<Logger> = OnceLock::new();

/// The process-wide logger, if one has been set.
#[must_use]
pub fn global() -> Option<&'static Logger> {
    GLOBAL.get()
}

/// Install a global logger, returning `Err(logger)` if one is already set.
pub fn set_global(logger: Logger) -> Result<(), Logger> {
    GLOBAL.set(logger)
}

/// Build a default logger (stdout sink, `Info` threshold) and install it as
/// the global logger. If a global logger already exists it is left untouched.
///
/// The [`XOSLOG`](crate::DEFAULT_FILTER_ENV) environment variable, if set, is
/// honoured for per-target level tuning (see [`TargetFilter`]).
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the logger cannot be built.
pub fn init_default() -> std::io::Result<()> {
    let logger = LoggerBuilder::new().build()?;
    let _ = GLOBAL.set(logger);
    Ok(())
}

/// Build a `Vec<Field>` for use with the structured-logging macro variants.
///
/// # Example
///
/// ```no_run
/// # use xoslog::{fields, log_info, init_default};
/// # fn main() { let _ = init_default();
/// log_info!([fields!(user = "alice", attempts = 3, ok = true)], "login attempt");
/// # }
/// ```
#[macro_export]
macro_rules! fields {
    ($($key:ident = $value:expr),* $(,)?) => {{
        vec![
            $($crate::Field::new(stringify!($key), $value),)*
        ]
    }};
}

/// Log at an explicit level via the global logger.
///
/// The level expression must be a [`Level`] value, e.g.
/// `xoslog::log!(xoslog::Level::Info, "hi {}", 1)`. If no global logger has
/// been installed the macro is a no-op.
///
/// A bracketed list of [`Field`]s may be passed before the message to attach
/// structured data, e.g. `log!(Level::Info, [fields!(user = "alice")], "hi")`.
#[macro_export]
macro_rules! log {
    ($level:expr, [$fields:expr], $($arg:tt)*) => {{
        if let Some(__xoslog) = $crate::global() {
            if __xoslog.is_enabled_for($level, module_path!()) {
                let mut __entry = $crate::LogEntry::new(
                    $level,
                    format!($($arg)*),
                    module_path!(),
                    file!(),
                    line!(),
                );
                __entry.fields = $fields;
                __xoslog.log(__entry);
            }
        }
    }};
    ($level:expr, $($arg:tt)*) => {{
        if let Some(__xoslog) = $crate::global() {
            if __xoslog.is_enabled_for($level, module_path!()) {
                __xoslog.log($crate::LogEntry::new(
                    $level,
                    format!($($arg)*),
                    module_path!(),
                    file!(),
                    line!(),
                ));
            }
        }
    }};
}

/// Log at [`Level::Trace`] via the global logger.
#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => { $crate::log!($crate::Level::Trace, $($arg)*) };
}

/// Log at [`Level::Debug`] via the global logger.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::log!($crate::Level::Debug, $($arg)*) };
}

/// Log at [`Level::Info`] via the global logger.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log!($crate::Level::Info, $($arg)*) };
}

/// Log at [`Level::Warn`] via the global logger.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::log!($crate::Level::Warn, $($arg)*) };
}

/// Log at [`Level::Error`] via the global logger.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::log!($crate::Level::Error, $($arg)*) };
}
