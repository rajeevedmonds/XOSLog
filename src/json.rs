//! Flat JSON formatting of log records.
//!
//! Each record is serialized as a single flat JSON object on one line, e.g.
//!
//! ```json
//! {"ts":"2026-08-14T04:00:00.000000Z","level":"INFO","msg":"user login","file":"src/main.rs","line":42,"target":"my_app","user":"alice"}
//! ```
//!
//! This is the shape aggregators such as Loki, ELK and Datadog expect: every
//! line is one JSON document, so no server-side parser is needed.

use std::fmt::Write as _;

use crate::entry::{FieldValue, LogEntry};

/// Write `entry` as a single flat JSON object followed by a newline into `out`.
///
/// When `include_location` is false the `file`/`line`/`target` members are
/// omitted.
pub fn write_record(entry: &LogEntry, include_location: bool, out: &mut Vec<u8>) {
    let mut line = String::with_capacity(96 + entry.message.len());
    let _ = write!(
        line,
        "{{\"ts\":\"{}\",\"level\":\"{}\",\"msg\":",
        entry.timestamp,
        entry.level.as_str()
    );
    push_string(&mut line, &entry.message);

    if include_location {
        let _ = write!(line, ",\"file\":");
        push_string(&mut line, entry.file);
        let _ = write!(line, ",\"line\":{},\"target\":", entry.line);
        push_string(&mut line, entry.target);
    }

    for field in &entry.fields {
        let _ = write!(line, ",");
        push_string(&mut line, &field.key);
        line.push(':');
        push_value(&mut line, &field.value);
    }

    line.push('}');
    line.push('\n');
    out.extend_from_slice(line.as_bytes());
}

/// Append `value` serialized as a JSON value.
fn push_value(out: &mut String, value: &FieldValue) {
    match value {
        FieldValue::Str(s) => push_string(out, s),
        FieldValue::Int(i) => {
            let _ = write!(out, "{i}");
        }
        FieldValue::Float(f) => {
            if f.is_finite() {
                let _ = write!(out, "{f}");
            } else {
                // JSON has no NaN/infinity; emit null instead of invalid JSON.
                out.push_str("null");
            }
        }
        FieldValue::Bool(b) => {
            let _ = write!(out, "{b}");
        }
        FieldValue::Null => out.push_str("null"),
    }
}

/// Append `value` as a JSON string literal, escaping as needed.
fn push_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Field;
    use crate::level::Level;
    use crate::time::Timestamp;

    fn entry() -> LogEntry {
        let mut e = LogEntry::new(
            Level::Info,
            "hello \"world\"".into(),
            "my_app::server",
            "src/server.rs",
            42,
        );
        e.timestamp = Timestamp {
            year: 2026,
            month: 8,
            day: 14,
            hour: 4,
            minute: 0,
            second: 0,
            microsecond: 0,
            offset_seconds: 0,
        };
        e
    }

    #[test]
    fn flat_shape_with_fields() {
        let mut e = entry();
        e.fields.push(Field::str("user", "alice"));
        e.fields.push(Field::int("attempts", 3));
        e.fields.push(Field::bool("ok", true));
        e.fields.push(Field::float("lat", 1.5));
        e.fields.push(Field::new("extra", FieldValue::Null));

        let mut out = Vec::new();
        write_record(&e, true, &mut out);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(
            s,
            "{\"ts\":\"2026-08-14T04:00:00.000000Z\",\"level\":\"INFO\",\"msg\":\"hello \\\"world\\\"\",\"file\":\"src/server.rs\",\"line\":42,\"target\":\"my_app::server\",\"user\":\"alice\",\"attempts\":3,\"ok\":true,\"lat\":1.5,\"extra\":null}\n"
        );
    }

    #[test]
    fn escaping() {
        let mut e = entry();
        e.fields.push(Field::str("path", "a\nb\t\\\"\u{01}c"));
        let mut out = Vec::new();
        write_record(&e, false, &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("\\n"), "newline must be escaped: {s}");
        assert!(s.contains("\\t"), "tab must be escaped: {s}");
        assert!(s.contains("\\\\\\\""), "backslash and quote escaped: {s}");
        assert!(s.contains("\\u0001"), "control char escaped: {s}");
        assert!(
            !s.contains('\u{01}'),
            "raw control char must not appear: {s}"
        );
    }

    #[test]
    fn location_omitted_when_disabled() {
        let mut out = Vec::new();
        write_record(&entry(), false, &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("file"), "file member omitted: {s}");
        assert!(!s.contains("line"), "line member omitted: {s}");
        assert!(!s.contains("target"), "target member omitted: {s}");
    }

    #[test]
    fn non_finite_float_is_null() {
        let mut e = entry();
        e.fields.push(Field::float("nan", f64::NAN));
        e.fields.push(Field::float("inf", f64::INFINITY));
        let mut out = Vec::new();
        write_record(&e, false, &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("\"nan\":null"), "got: {s}");
        assert!(s.contains("\"inf\":null"), "got: {s}");
        assert!(!s.contains("NaN"), "invalid JSON must be avoided: {s}");
    }
}
