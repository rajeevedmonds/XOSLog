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

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod entry;
mod level;
mod logger;
mod sink;
mod syslog;
mod time;

pub use entry::LogEntry;
pub use level::Level;
pub use logger::{
    global, init_default, set_global, Backpressure, Logger, LoggerBuilder,
    DEFAULT_CHANNEL_CAPACITY, DEFAULT_MAX_FILE_SIZE,
};
pub use sink::{FileSink, Sink};
pub use syslog::{Facility, SyslogSink, DEFAULT_SYSLOG_SOCKETS};
pub use time::Timestamp;
