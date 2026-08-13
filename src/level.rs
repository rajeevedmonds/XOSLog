//! Log severity levels.

/// Severity levels supported by the logger.
///
/// The ordering is `Trace < Debug < Info < Warn < Error < Off`. A logger
/// configured with a threshold level only records messages at or above that
/// level. `Off` disables every record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    /// Finest-grained diagnostic messages.
    Trace = 0,
    /// Detailed information useful for debugging.
    Debug = 1,
    /// General informational messages.
    Info = 2,
    /// Recoverable problems that deserve attention.
    Warn = 3,
    /// Errors that should be fixed but do not stop the program.
    Error = 4,
    /// Disables all logging.
    Off = 5,
}

impl Level {
    /// The canonical uppercase name of the level, e.g. `"INFO"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Off => "OFF",
        }
    }

    /// Parse a level from its (case-insensitive) name.
    ///
    /// Accepts `trace`, `debug`, `info`, `warn`/`warning`, `error` and `off`.
    #[must_use]
    pub fn parse(input: &str) -> Option<Level> {
        match input.trim().to_ascii_lowercase().as_str() {
            "trace" => Some(Level::Trace),
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" => Some(Level::Error),
            "off" => Some(Level::Off),
            _ => None,
        }
    }
}

impl core::fmt::Display for Level {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::Level;

    #[test]
    fn ordering() {
        assert!(Level::Trace < Level::Debug);
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
        assert!(Level::Error < Level::Off);
    }

    #[test]
    fn parse_cases() {
        assert_eq!(Level::parse("INFO"), Some(Level::Info));
        assert_eq!(Level::parse(" info "), Some(Level::Info));
        assert_eq!(Level::parse("WARNING"), Some(Level::Warn));
        assert_eq!(Level::parse("Error"), Some(Level::Error));
        assert_eq!(Level::parse("verbose"), None);
        assert_eq!(Level::parse(""), None);
    }

    #[test]
    fn str_roundtrip() {
        for level in [
            Level::Trace,
            Level::Debug,
            Level::Info,
            Level::Warn,
            Level::Error,
            Level::Off,
        ] {
            assert_eq!(Level::parse(level.as_str()), Some(level));
        }
    }
}
