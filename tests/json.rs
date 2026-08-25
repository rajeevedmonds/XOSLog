use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use xoslog::{fields, log_info, set_global, Field, FieldValue, Level, LogEntry, LoggerBuilder};

fn temp_file(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("xoslog-json-{tag}-{}-{nanos}", std::process::id()))
}

fn read_lines(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn json_records_are_flat_objects_per_line() {
    let path = temp_file("shape");
    let logger = LoggerBuilder::new()
        .json()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(
        LogEntry::new(
            Level::Info,
            "user login".to_string(),
            module_path!(),
            file!(),
            line!(),
        )
        .field("user", "alice")
        .field("attempts", 3),
    );
    logger.flush();

    let lines = read_lines(&path);
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert!(line.starts_with('{') && line.ends_with('}'), "got: {line}");
    assert!(line.contains("\"level\":\"INFO\""), "level: {line}");
    assert!(line.contains("\"msg\":\"user login\""), "msg: {line}");
    assert!(line.contains("\"ts\":\""), "ts: {line}");
    assert!(line.contains("\"file\":\"tests/json.rs\""), "file: {line}");
    assert!(line.contains("\"target\":"), "target: {line}");
    assert!(line.contains("\"user\":\"alice\""), "field: {line}");
    assert!(line.contains("\"attempts\":3"), "field: {line}");
}

#[test]
fn json_escapes_special_characters() {
    let path = temp_file("escape");
    let logger = LoggerBuilder::new()
        .json()
        .include_location(false)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(
        LogEntry::new(Level::Info, "line1\nline2".to_string(), "", "", 0)
            .field("path", "a\"b\\c\td")
            .field("raw_control", format!("x{}y", '\u{01}')),
    );
    logger.flush();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("\\n"), "newline escaped: {content}");
    assert!(
        content.contains("a\\\"b\\\\c\\td"),
        "quotes/slash/tab: {content}"
    );
    assert!(
        content.contains("\\u0001"),
        "control char escaped: {content}"
    );
    assert!(
        !content.contains("\u{01}"),
        "raw control char present: {content}"
    );
    assert_eq!(content.lines().count(), 1, "record must stay on one line");
}

#[test]
fn json_supports_typed_values() {
    let path = temp_file("typed");
    let logger = LoggerBuilder::new()
        .json()
        .include_location(false)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(
        LogEntry::new(Level::Warn, "metrics".to_string(), "", "", 0).with_fields(vec![
            Field::int("count", -7),
            Field::float("ratio", 0.25),
            Field::bool("healthy", false),
            Field::new("missing", FieldValue::Null),
            Field::str("big", u64::MAX.to_string()),
        ]),
    );
    logger.flush();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("\"count\":-7"), "int: {content}");
    assert!(content.contains("\"ratio\":0.25"), "float: {content}");
    assert!(content.contains("\"healthy\":false"), "bool: {content}");
    assert!(content.contains("\"missing\":null"), "null: {content}");
    assert!(
        content.contains("\"big\":\"18446744073709551615\""),
        "big u64: {content}"
    );
}

#[test]
fn fields_macro_and_global_logger() {
    let path = temp_file("global");
    let logger = LoggerBuilder::new()
        .json()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();
    assert!(set_global(logger).is_ok());

    log_info!([fields!(user = "bob", score = 9)], "scored");
    if let Some(g) = xoslog::global() {
        g.flush();
    }

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("\"level\":\"INFO\""), "level: {content}");
    assert!(content.contains("\"msg\":\"scored\""), "msg: {content}");
    assert!(
        content.contains("\"user\":\"bob\""),
        "field user: {content}"
    );
    assert!(content.contains("\"score\":9"), "field score: {content}");
}

#[test]
fn fields_are_ignored_by_plain_text_sinks() {
    let path = temp_file("plain");
    let logger = LoggerBuilder::new()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(
        LogEntry::new(Level::Info, "hello".to_string(), "", "", 0)
            .field("user", "alice")
            .field("attempts", 3),
    );
    logger.flush();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains(" [INFO] hello"), "plain format: {content}");
    assert!(
        !content.contains("alice"),
        "fields must not leak into plain text: {content}"
    );
    assert!(
        !content.contains("attempts"),
        "fields must not leak into plain text: {content}"
    );
}

#[test]
fn json_records_survive_concurrency() {
    let path = temp_file("threads");
    let logger = LoggerBuilder::new()
        .json()
        .channel_capacity(4096)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    let mut handles = Vec::new();
    for t in 0..8 {
        let logger = logger.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..100 {
                logger.log(
                    LogEntry::new(Level::Info, "concurrent".to_string(), "", "", 0)
                        .field("thread", t)
                        .field("seq", i),
                );
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    logger.shutdown();

    let lines = read_lines(&path);
    assert_eq!(lines.len(), 800);
    for line in &lines {
        assert!(line.starts_with('{') && line.ends_with('}'), "got: {line}");
        assert!(line.contains("\"msg\":\"concurrent\""), "got: {line}");
        assert!(line.contains("\"thread\":"), "got: {line}");
        assert!(line.contains("\"seq\":"), "got: {line}");
    }
}
