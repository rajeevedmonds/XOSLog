//! Log records passed from callers to the writer thread, including optional
//! structured fields.

use crate::level::Level;
use crate::time::Timestamp;

/// A typed value carried by a structured log field.
///
/// Values are serialized by the JSON formatter; aggregators such as ELK,
/// Loki and Datadog can consume them without a separate parser.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// A string value.
    Str(String),
    /// A signed 64-bit integer.
    Int(i64),
    /// A 64-bit floating point number.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// An explicit JSON `null`.
    Null,
}

impl FieldValue {
    /// Whether this value is a string.
    #[must_use]
    pub const fn is_string(&self) -> bool {
        matches!(self, FieldValue::Str(_))
    }
}

impl From<String> for FieldValue {
    fn from(value: String) -> Self {
        FieldValue::Str(value)
    }
}

impl From<&str> for FieldValue {
    fn from(value: &str) -> Self {
        FieldValue::Str(value.to_string())
    }
}

impl From<&String> for FieldValue {
    fn from(value: &String) -> Self {
        FieldValue::Str(value.clone())
    }
}

impl From<char> for FieldValue {
    fn from(value: char) -> Self {
        FieldValue::Str(value.to_string())
    }
}

impl From<bool> for FieldValue {
    fn from(value: bool) -> Self {
        FieldValue::Bool(value)
    }
}

impl From<i8> for FieldValue {
    fn from(value: i8) -> Self {
        FieldValue::Int(i64::from(value))
    }
}

impl From<i16> for FieldValue {
    fn from(value: i16) -> Self {
        FieldValue::Int(i64::from(value))
    }
}

impl From<i32> for FieldValue {
    fn from(value: i32) -> Self {
        FieldValue::Int(i64::from(value))
    }
}

impl From<i64> for FieldValue {
    fn from(value: i64) -> Self {
        FieldValue::Int(value)
    }
}

impl From<isize> for FieldValue {
    fn from(value: isize) -> Self {
        FieldValue::Int(value as i64)
    }
}

impl From<u8> for FieldValue {
    fn from(value: u8) -> Self {
        FieldValue::Int(i64::from(value))
    }
}

impl From<u16> for FieldValue {
    fn from(value: u16) -> Self {
        FieldValue::Int(i64::from(value))
    }
}

impl From<u32> for FieldValue {
    fn from(value: u32) -> Self {
        FieldValue::Int(i64::from(value))
    }
}

impl From<u64> for FieldValue {
    fn from(value: u64) -> Self {
        // Preserve the full value as a string when it exceeds i64::MAX.
        match i64::try_from(value) {
            Ok(v) => FieldValue::Int(v),
            Err(_) => FieldValue::Str(value.to_string()),
        }
    }
}

impl From<usize> for FieldValue {
    fn from(value: usize) -> Self {
        FieldValue::Int(value as i64)
    }
}

impl From<f32> for FieldValue {
    fn from(value: f32) -> Self {
        FieldValue::Float(f64::from(value))
    }
}

impl From<f64> for FieldValue {
    fn from(value: f64) -> Self {
        FieldValue::Float(value)
    }
}

/// A single key/value pair attached to a log record.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// Field name, used as the JSON key.
    pub key: String,
    /// Field value, serialized as a typed JSON value.
    pub value: FieldValue,
}

impl Field {
    /// Create a field with an arbitrary typed value.
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<FieldValue>) -> Field {
        Field {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Create a string field.
    #[must_use]
    pub fn str(key: impl Into<String>, value: impl Into<String>) -> Field {
        Field::new(key, FieldValue::Str(value.into()))
    }

    /// Create an integer field.
    #[must_use]
    pub fn int(key: impl Into<String>, value: i64) -> Field {
        Field::new(key, FieldValue::Int(value))
    }

    /// Create a floating point field.
    #[must_use]
    pub fn float(key: impl Into<String>, value: f64) -> Field {
        Field::new(key, FieldValue::Float(value))
    }

    /// Create a boolean field.
    #[must_use]
    pub fn bool(key: impl Into<String>, value: bool) -> Field {
        Field::new(key, FieldValue::Bool(value))
    }
}

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
    /// Structured key/value fields attached to the record.
    pub fields: Vec<Field>,
}

impl LogEntry {
    /// Create a new log record with no structured fields.
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
            fields: Vec::new(),
        }
    }

    /// Attach a typed field to the record.
    #[must_use]
    pub fn field(mut self, key: impl Into<String>, value: impl Into<FieldValue>) -> LogEntry {
        self.fields.push(Field::new(key, value));
        self
    }

    /// Attach several fields to the record at once.
    #[must_use]
    pub fn with_fields(mut self, fields: impl IntoIterator<Item = Field>) -> LogEntry {
        self.fields.extend(fields);
        self
    }

    /// Merge thread-local context fields into the record.
    ///
    /// Fields already present on the record (explicitly, via [`field`] or
    /// [`with_fields`]) win over context fields with the same key; remaining
    /// context fields are appended after them. Records without context are
    /// returned unchanged.
    ///
    /// [`field`]: LogEntry::field
    /// [`with_fields`]: LogEntry::with_fields
    #[must_use]
    pub fn merge_context(mut self, context: Vec<Field>) -> LogEntry {
        for field in context {
            if !self.fields.iter().any(|existing| existing.key == field.key) {
                self.fields.push(field);
            }
        }
        self
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_helpers() {
        let f = Field::str("user", "alice");
        assert_eq!(f.key, "user");
        assert_eq!(f.value, FieldValue::Str("alice".to_string()));
        assert_eq!(Field::int("n", 3).value, FieldValue::Int(3));
        assert_eq!(Field::bool("ok", true).value, FieldValue::Bool(true));
        assert_eq!(Field::float("r", 1.5).value, FieldValue::Float(1.5));
    }

    #[test]
    fn conversions() {
        assert_eq!(FieldValue::from("x"), FieldValue::Str("x".to_string()));
        assert_eq!(FieldValue::from(42i32), FieldValue::Int(42));
        assert_eq!(FieldValue::from(1.5f64), FieldValue::Float(1.5));
        assert_eq!(FieldValue::from(true), FieldValue::Bool(true));
        // u64 values beyond i64::MAX degrade to strings, never truncate.
        let big = u64::MAX;
        assert_eq!(FieldValue::from(big), FieldValue::Str(big.to_string()));
    }

    #[test]
    fn entry_fields_chain() {
        let entry = LogEntry::new(Level::Info, "msg".into(), "", "", 0)
            .field("user", "alice")
            .field("count", 3);
        assert_eq!(entry.fields.len(), 2);
        assert_eq!(entry.fields[0].key, "user");
        assert_eq!(entry.fields[1].value, FieldValue::Int(3));
    }
}
