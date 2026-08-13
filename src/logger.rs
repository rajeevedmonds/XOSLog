//! The `Logger`, its builder, the background writer thread, the global logger
//! and the `log_*!` macros.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use crate::entry::LogEntry;
use crate::level::Level;
use crate::sink::{FileSink, Sink, StderrSink, StdoutSink};
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
    time_offset_seconds: i32,
    backpressure: Backpressure,
    handle: Mutex<Option<JoinHandle<()>>>,
    closed: AtomicBool,
    dropped: AtomicUsize,
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
    /// Whether records at `level` will be recorded by this logger.
    #[must_use]
    pub fn is_enabled(&self, level: Level) -> bool {
        level != Level::Off && self.inner.level != Level::Off && level >= self.inner.level
    }

    /// The threshold level of this logger.
    #[must_use]
    pub fn level(&self) -> Level {
        self.inner.level
    }

    /// Enqueue a log record.
    ///
    /// The record's timestamp is replaced with the current wall-clock time
    /// (using this logger's configured offset). Records below the logger's
    /// threshold are discarded. If the writer thread has died, the record is
    /// written synchronously to standard error as a last resort.
    pub fn log(&self, mut entry: LogEntry) {
        if self.inner.closed.load(Ordering::Acquire) {
            return;
        }
        if !self.is_enabled(entry.level) {
            return;
        }
        entry.timestamp = Timestamp::now(self.inner.time_offset_seconds);

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
            let _ = emergency_write(&entry);
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
    sink: SinkSource,
}

impl Default for LoggerBuilder {
    fn default() -> Self {
        LoggerBuilder {
            level: Level::Info,
            capacity: DEFAULT_CHANNEL_CAPACITY,
            backpressure: Backpressure::Block,
            include_location: true,
            time_offset_seconds: 0,
            sink: SinkSource::Stdout,
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
        self.sink = SinkSource::Stdout;
        self
    }

    /// Write to standard error.
    #[must_use]
    pub fn to_stderr(mut self) -> LoggerBuilder {
        self.sink = SinkSource::Stderr;
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
        self.sink = SinkSource::File(path.as_ref().to_path_buf(), max_size, max_backups);
        Ok(self)
    }

    /// Use a caller-provided sink.
    #[must_use]
    pub fn sink(mut self, sink: impl Sink + 'static) -> LoggerBuilder {
        self.sink = SinkSource::Custom(Box::new(sink));
        self
    }

    /// Build the logger: open the sink and spawn the writer thread.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the sink cannot be opened or the
    /// writer thread cannot be spawned.
    pub fn build(self) -> std::io::Result<Logger> {
        let sink: Box<dyn Sink> = match self.sink {
            SinkSource::Stdout => Box::new(StdoutSink),
            SinkSource::Stderr => Box::new(StderrSink),
            SinkSource::File(path, max_size, max_backups) => {
                Box::new(FileSink::open(path, max_size, max_backups)?)
            }
            SinkSource::Custom(sink) => sink,
        };

        let (tx, rx) = mpsc::sync_channel(self.capacity);
        let include_location = self.include_location;
        let handle = std::thread::Builder::new()
            .name("xoslog-writer".to_string())
            .spawn(move || writer_loop(rx, sink, include_location))?;

        let inner = Arc::new(Inner {
            tx: Mutex::new(tx),
            level: self.level,
            time_offset_seconds: self.time_offset_seconds,
            backpressure: self.backpressure,
            handle: Mutex::new(Some(handle)),
            closed: AtomicBool::new(false),
            dropped: AtomicUsize::new(0),
        });

        Ok(Logger { inner })
    }
}

/// The writer thread: drains the queue, formats and writes records.
fn writer_loop(rx: Receiver<Message>, mut sink: Box<dyn Sink>, include_location: bool) {
    let mut consecutive_errors = 0usize;
    let mut degraded = false;
    let mut running = true;

    while running {
        let msg = match rx.recv() {
            Ok(msg) => msg,
            Err(_) => break,
        };

        let stop = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match msg {
            Message::Log(entry) => {
                let mut buf = Vec::with_capacity(96 + entry.message.len());
                format_record(&entry, include_location, &mut buf);
                if degraded {
                    let mut err = std::io::stderr().lock();
                    let _ = err.write_all(&buf);
                    let _ = err.flush();
                    return false;
                }
                if sink.write_bytes(&buf).is_ok() {
                    consecutive_errors = 0;
                } else {
                    consecutive_errors += 1;
                    if consecutive_errors >= MAX_CONSECUTIVE_WRITE_ERRORS {
                        degraded = true;
                        let mut err = std::io::stderr().lock();
                        let _ = err
                            .write_all(b"[xoslog] primary sink failed, falling back to stderr\n");
                    }
                }
                false
            }
            Message::Flush(ack) => {
                let _ = sink.flush();
                let _ = ack.send(());
                false
            }
            Message::Shutdown(ack) => {
                let _ = sink.flush();
                let _ = ack.send(());
                true
            }
        })) {
            Ok(stop) => stop,
            Err(_) => {
                degraded = true;
                false
            }
        };

        running = !stop;
    }

    let _ = sink.flush();
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
fn emergency_write(entry: &LogEntry) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(96 + entry.message.len());
    format_record(entry, false, &mut buf);
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
/// # Errors
///
/// Returns an [`std::io::Error`] if the logger cannot be built.
pub fn init_default() -> std::io::Result<()> {
    let logger = LoggerBuilder::new().build()?;
    let _ = GLOBAL.set(logger);
    Ok(())
}

/// Log at an explicit level via the global logger.
///
/// The level expression must be a [`Level`] value, e.g.
/// `xoslog::log!(xoslog::Level::Info, "hi {}", 1)`. If no global logger has
/// been installed the macro is a no-op.
#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)*) => {{
        if let Some(__xoslog) = $crate::global() {
            if __xoslog.is_enabled($level) {
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
