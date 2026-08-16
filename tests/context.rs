use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use xoslog::{
    capture_context, current_context, log_info, push_context, set_global, Field, Level, LogEntry,
    LoggerBuilder,
};

fn temp_file(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "xoslog-context-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

fn read_lines(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn context_is_empty_by_default() {
    assert!(current_context().is_empty());
}

#[test]
fn json_records_carry_context_within_scope_only() {
    let path = temp_file("scope");
    let logger = LoggerBuilder::new()
        .json()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    logger.info("before scope");
    {
        let _guard = push_context([
            Field::str("request_id", "abc-123"),
            Field::str("user_id", "u42"),
        ]);
        logger.info("inside scope");
    }
    logger.info("after scope");
    logger.flush();

    let lines = read_lines(&path);
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("\"msg\":\"before scope\""));
    assert!(
        !lines[0].contains("request_id"),
        "context must not leak outside scope: {}",
        lines[0]
    );
    assert!(lines[1].contains("\"request_id\":\"abc-123\""), "{}", lines[1]);
    assert!(lines[1].contains("\"user_id\":\"u42\""), "{}", lines[1]);
    assert!(lines[2].contains("\"msg\":\"after scope\""));
    assert!(
        !lines[2].contains("request_id"),
        "context must be removed after guard drop: {}",
        lines[2]
    );
}

#[test]
fn plain_text_sinks_ignore_context() {
    let path = temp_file("plain");
    let logger = LoggerBuilder::new()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    let _guard = push_context([Field::str("request_id", "abc-123")]);
    logger.info("inside scope");
    logger.flush();

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains(" [INFO] inside scope"));
    assert!(
        !content.contains("abc-123"),
        "context must not leak into plain text: {content}"
    );
    assert!(!content.contains("request_id"), "{content}");
}

#[test]
fn explicit_record_fields_win_over_context() {
    let path = temp_file("precedence");
    let logger = LoggerBuilder::new()
        .json()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    let _guard = push_context([Field::str("request_id", "from-context")]);
    logger.log(
        LogEntry::new(Level::Info, "override".to_string(), "", "", 0)
            .field("request_id", "from-record"),
    );
    logger.flush();

    let lines = read_lines(&path);
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert!(
        line.contains("\"request_id\":\"from-record\""),
        "explicit field must win: {line}"
    );
    assert!(
        !line.contains("from-context"),
        "context value must not appear: {line}"
    );
    assert_eq!(
        line.matches("\"request_id\"").count(),
        1,
        "request_id must appear exactly once: {line}"
    );
}

#[test]
fn nested_scopes_innermost_wins() {
    let path = temp_file("nested");
    let logger = LoggerBuilder::new()
        .json()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    let outer = push_context([Field::str("request_id", "outer"), Field::str("user", "alice")]);
    {
        let inner = push_context([Field::str("request_id", "inner")]);
        logger.info("in inner");
        drop(inner);
        logger.info("in outer");
    }
    drop(outer);
    logger.flush();

    let lines = read_lines(&path);
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].contains("\"request_id\":\"inner\""),
        "innermost wins: {}",
        lines[0]
    );
    assert!(lines[0].contains("\"user\":\"alice\""), "{}", lines[0]);
    assert!(
        lines[1].contains("\"request_id\":\"outer\""),
        "outer restored after inner guard drop: {}",
        lines[1]
    );
}

#[test]
fn global_macros_carry_context() {
    let path = temp_file("global");
    let logger = LoggerBuilder::new()
        .json()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();
    assert!(set_global(logger).is_ok());

    let _guard = push_context([Field::str("trace_id", "t1")]);
    log_info!("with context");
    if let Some(g) = xoslog::global() {
        g.flush();
    }

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("\"msg\":\"with context\""), "{content}");
    assert!(content.contains("\"trace_id\":\"t1\""), "{content}");
}

#[test]
fn context_transfers_to_spawned_thread() {
    let path = temp_file("transfer");
    let logger = LoggerBuilder::new()
        .json()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    let _guard = push_context([Field::str("request_id", "abc-123")]);
    let snapshot = capture_context();

    let worker = {
        let logger = logger.clone();
        std::thread::spawn(move || {
            // Fresh thread: no inherited context.
            assert!(current_context().is_empty());
            let _guard = snapshot.enter();
            logger.info("in worker");
        })
    };
    worker.join().unwrap();

    logger.info("in main");
    logger.flush();

    let lines = read_lines(&path);
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].contains("\"request_id\":\"abc-123\""),
        "transferred context in worker: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("\"request_id\":\"abc-123\""),
        "main thread context still active: {}",
        lines[1]
    );
}

#[test]
fn context_captured_at_log_time() {
    let path = temp_file("capture-time");
    let logger = LoggerBuilder::new()
        .json()
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    let guard = push_context([Field::str("request_id", "first")]);
    logger.info("recorded under first");
    // Context changes before the writer drains; the enqueued record must not.
    let _guard2 = push_context([Field::str("request_id", "second")]);
    drop(guard);
    logger.info("recorded under second");
    logger.flush();

    let lines = read_lines(&path);
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].contains("\"request_id\":\"first\""),
        "context must be captured at log() time: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("\"request_id\":\"second\""),
        "{}",
        lines[1]
    );
}
