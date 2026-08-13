use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use xoslog::{Level, LogEntry, LoggerBuilder, Timestamp};

fn temp_file(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("xoslog-basic-{tag}-{}-{nanos}", std::process::id()))
}

#[test]
fn logs_to_file_with_full_format() {
    let path = temp_file("format");
    let logger = LoggerBuilder::new()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.log(LogEntry::new(
        Level::Info,
        "hello world".to_string(),
        module_path!(),
        file!(),
        line!(),
    ));
    logger.warn("careful now");
    logger.flush();

    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);

    let first = lines[0];
    assert!(
        first.starts_with('2'),
        "line should start with a year: {first}"
    );
    assert!(first.contains(" [INFO] "), "missing INFO tag: {first}");
    assert!(first.contains("hello world"), "missing message: {first}");
    assert!(
        first.contains("tests/basic.rs"),
        "missing location: {first}"
    );
    assert!(first.contains("@ "), "missing module: {first}");
}

#[test]
fn level_filtering() {
    let path = temp_file("levels");
    let logger = LoggerBuilder::new()
        .level(Level::Warn)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.trace("t");
    logger.debug("d");
    logger.info("i");
    logger.warn("w");
    logger.error("e");
    logger.flush();

    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "only warn+error should be recorded: {content}"
    );
    assert!(content.contains(" [WARN] w"));
    assert!(content.contains(" [ERROR] e"));
}

#[test]
fn off_level_suppresses_everything() {
    let path = temp_file("off");
    let logger = LoggerBuilder::new()
        .level(Level::Off)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.error("should not appear");
    logger.flush();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.is_empty());
}

#[test]
fn location_can_be_disabled() {
    let path = temp_file("noloc");
    let logger = LoggerBuilder::new()
        .include_location(false)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.info("bare");
    logger.flush();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains(" [INFO] bare"));
    assert!(!content.contains('('));
    assert!(!content.contains('@'));
}

#[test]
fn timestamp_round_trip() {
    let ts = Timestamp::now(0).to_string();
    assert_eq!(ts.len(), 27);
    assert!(ts.ends_with('Z'));
    let parts: Vec<&str> = ts.split('T').collect();
    assert_eq!(parts.len(), 2);
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    assert_eq!(date_parts.len(), 3);
    let _year: i32 = date_parts[0].parse().unwrap();
    let _month: u8 = date_parts[1].parse().unwrap();
    let _day: u8 = date_parts[2].parse().unwrap();
}

#[test]
fn log_call_after_shutdown_is_noop() {
    let path = temp_file("shutdown");
    let logger = LoggerBuilder::new()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.info("before");
    let clone = logger.clone();
    clone.shutdown();
    logger.info("after");
    // give a moment for any straggler (there should be none)
    std::thread::sleep(std::time::Duration::from_millis(20));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("before"));
    assert!(!content.contains("after"));
}
