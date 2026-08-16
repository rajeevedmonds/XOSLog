//! `xoslog` is a thread-safe, robust logging library for Linux written in
//! pure Rust.
//!
//! The crate has **zero runtime dependencies** and contains **no `unsafe`
//! code** (`#![forbid(unsafe_code)]` is enforced at compile time).
//!
//! # Design
//!
//! Logging is asynchronous: callers push formatted records onto a bounded,
//! thread-safe queue and a dedicated writer thread drains the queue, formats
//! records and writes them to the configured sink (standard output, standard
//! error, a size-rotating file, or the syslog daemon). This keeps logging
//! calls cheap and lock-free for callers while a single serialized writer
//! guarantees that records from any number of threads are never interleaved.
//!
//! The library is *robust*:
//!
//! - The writer thread survives panics and keeps serving the queue.
//! - If the primary sink starts failing, the writer automatically falls back
//!   to standard error after a configurable number of consecutive failures.
//! - If the writer thread dies, callers fall back to writing synchronously to
//!   standard error so records are never silently lost.
//! - A bounded queue with an explicit [`Backpressure`] policy prevents
//!   unbounded memory growth.
//! - [`Logger::flush`] and [`Logger::shutdown`] provide deterministic
//!   draining and clean teardown.
//!
//! # Example
//!
//! ```no_run
//! use xoslog::{init_default, log_info, log_error, Level};
//!
//! fn main() {
//!     // Optional global logger so the log_* macros work.
//!     init_default().unwrap();
//!
//!     log_info!("application starting");
//!     log_error!("something went wrong: {}", 42);
//!
//!     if let Some(logger) = xoslog::global() {
//!         logger.flush();
//!     }
//! }
//! ```
//!
//! # Quick start with a file sink
//!
//! ```no_run
//! use xoslog::{Level, LoggerBuilder};
//!
//! let logger = LoggerBuilder::new()
//!     .level(Level::Info)
//!     .to_file("/var/log/app.log", 10 * 1024 * 1024, 3)
//!     .unwrap()
//!     .build()
//!     .unwrap();
//!
//! logger.info("hello from xoslog");
//! logger.flush();
//! ```
//!
//! # Syslog
//!
//! ```no_run
//! use xoslog::{Facility, Level, LoggerBuilder};
//!
//! let logger = LoggerBuilder::new()
//!     .level(Level::Info)
//!     .to_syslog(Facility::Daemon)
//!     .build()
//!     .unwrap();
//!
//! logger.warn("low memory");
//! logger.flush();
//! ```
//!
//! # Structured logging
//!
//! Turn on `.json()` to emit each record as a single flat JSON object per
//! line, ready for Loki, ELK and Datadog without a server-side parser.
//! Attach typed fields to any record:
//!
//! ```no_run
//! use xoslog::{Level, LoggerBuilder, LogEntry};
//!
//! let logger = LoggerBuilder::new()
//!     .json()
//!     .to_file("/var/log/app.log", 10 * 1024 * 1024, 3)
//!     .unwrap()
//!     .build()
//!     .unwrap();
//!
//! logger.log(
//!     LogEntry::new(Level::Info, "user login".to_string(), module_path!(), file!(), line!())
//!         .field("user", "alice")
//!         .field("attempts", 3),
//! );
//! logger.flush();
//! ```
//!
//! Records look like
//! `{"ts":"...","level":"INFO","msg":"user login","file":"...","line":N,"target":"...","user":"alice","attempts":3}`.
//! The `fields!` helper works with the global macros:
//!
//! ```no_run
//! use xoslog::{fields, log_info, init_default};
//!
//! # fn main() {
//! let _ = init_default();
//! log_info!([fields!(user = "bob", score = 9)], "scored");
//! # }
//! ```
//!
//! # Contextual logging (spans)
//!
//! Attach request/user/trace IDs to a scope so every record emitted inside it
//! automatically carries them as fields:
//!
//! ```no_run
//! # use xoslog::{init_default, log_info, push_context};
//! # fn main() { let _ = init_default();
//! let _guard = push_context([xoslog::Field::str("request_id", "abc-123")]);
//! log_info!("handling request"); // JSON output includes "request_id"
//! # }
//! ```
//!
//! Scopes nest (innermost wins) and the guard pops the scope on drop. Context
//! is thread-local; use [`capture_context`] and [`ContextSnapshot::enter`] to
//! transfer it to spawned threads.
//!
//! # Remote logging
//!
//! Send records to a remote Linux server as RFC 3164 syslog over UDP:
//!
//! ```no_run
//! use xoslog::{Facility, Level, LoggerBuilder};
//!
//! let logger = LoggerBuilder::new()
//!     .level(Level::Info)
//!     .to_remote_syslog("192.0.2.10", 514, Facility::Daemon)
//!     .unwrap()
//!     .build()
//!     .unwrap();
//!
//! logger.info("reaching across the network");
//! logger.flush();
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod context;
mod entry;
mod json;
mod level;
mod logger;
mod sink;
mod syslog;
mod time;

pub use context::{capture_context, current_context, push_context, ContextGuard, ContextSnapshot};
pub use entry::{Field, FieldValue, LogEntry};
pub use json::write_record;
pub use level::Level;
pub use logger::{
    global, init_default, set_global, Backpressure, Logger, LoggerBuilder,
    DEFAULT_CHANNEL_CAPACITY, DEFAULT_MAX_FILE_SIZE,
};
pub use sink::{FileSink, Sink};
pub use syslog::{Facility, RemoteSyslogSink, SyslogSink, DEFAULT_SYSLOG_SOCKETS};
pub use time::Timestamp;
