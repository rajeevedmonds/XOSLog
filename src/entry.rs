//! Log records passed from callers to the writer thread.

use crate::level::Level;
use crate::time::Timestamp;

/// A single log record produced by a caller and consumed by the writer thread.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Wall-clock time when the record was logged.
    pub timestamp: Timestamp,
    /// Severity level of the record.
    pub level: Level,
    /// Formatted message body.
    pub message: String,
    /// Module path where the record was created (e.g. `my_app::server`).
    pub target: &'static str,
    /// Source file where the record was created.
    pub file: &'static str,
    /// Line number in the source file.
    pub line: u32,
}

impl LogEntry {
    /// Create a new log record.
    ///
    /// The timestamp is a placeholder; [`Logger::log`] stamps the record with
    /// the real wall-clock time before enqueueing it.
    #[must_use]
    pub fn new(
        level: Level,
        message: String,
        target: &'static str,
        file: &'static str,
        line: u32,
    ) -> LogEntry {
        LogEntry {
            timestamp: Timestamp::now(0),
            level,
            message,
            target,
            file,
            line,
        }
    }

    /// Human-readable source location, e.g. `src/main.rs:42 @ my_app`.
    ///
    /// Returns an empty string when no source location is attached.
    #[must_use]
    pub fn location(&self) -> String {
        if self.file.is_empty() {
            String::new()
        } else {
            format!("{}:{} @ {}", self.file, self.line, self.target)
        }
    }
}
