use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use xoslog::{Backpressure, LoggerBuilder, Sink};

fn temp_file(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "xoslog-thread-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn concurrent_logging_loses_nothing() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 2000;

    let path = temp_file("noforce");
    let logger = LoggerBuilder::new()
        .channel_capacity(128)
        .backpressure(Backpressure::Block)
        .include_location(false)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    let mut handles = Vec::new();
    for t in 0..THREADS {
        let logger = logger.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..PER_THREAD {
                logger.info(format!("thread {t} message {i}"));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    logger.flush();
    assert_eq!(logger.dropped_message_count(), 0);

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content.lines().count(), THREADS * PER_THREAD);

    for t in 0..THREADS {
        assert!(
            content.contains(&format!("thread {t} message {}", PER_THREAD - 1)),
            "missing final message from thread {t}"
        );
    }
}

#[test]
fn concurrent_logging_with_drop_newest_never_blocks() {
    const THREADS: usize = 4;
    const PER_THREAD: usize = 500;

    let path = temp_file("drop");
    let logger = LoggerBuilder::new()
        .channel_capacity(16)
        .backpressure(Backpressure::DropNewest)
        .include_location(false)
        .to_file(&path, 0, 0)
        .unwrap()
        .build()
        .unwrap();

    let mut handles = Vec::new();
    for t in 0..THREADS {
        let logger = logger.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..PER_THREAD {
                logger.info(format!("thread {t} message {i}"));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    logger.flush();
    let written = std::fs::read_to_string(&path).unwrap().lines().count();
    let dropped = logger.dropped_message_count();
    assert_eq!(
        written + dropped,
        THREADS * PER_THREAD,
        "messages must be either written or counted as dropped"
    );
}

/// A sink that records bytes written, so tests can inspect them.
#[derive(Default)]
struct MemorySink {
    data: Arc<Mutex<Vec<u8>>>,
}

impl Sink for MemorySink {
    fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.data.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn custom_sink_receives_records() {
    let data = Arc::new(Mutex::new(Vec::new()));
    let sink = MemorySink {
        data: Arc::clone(&data),
    };

    let logger = LoggerBuilder::new()
        .include_location(false)
        .sink(sink)
        .build()
        .unwrap();

    logger.info("into memory");
    logger.flush();

    let content = String::from_utf8(data.lock().unwrap().clone()).unwrap();
    assert!(content.contains("into memory"));
}

/// A sink whose every write fails.
struct FailingSink {
    attempts: Arc<AtomicUsize>,
}

impl Sink for FailingSink {
    fn write_bytes(&mut self, _bytes: &[u8]) -> std::io::Result<()> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::other("boom"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn writer_falls_back_after_repeated_failures() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let sink = FailingSink {
        attempts: Arc::clone(&attempts),
    };

    let logger = LoggerBuilder::new()
        .include_location(false)
        .sink(sink)
        .build()
        .unwrap();

    // Far more than the 8 consecutive-failure threshold.
    for _ in 0..100 {
        logger.info("boom");
    }
    logger.flush();

    // After the threshold is crossed the writer stops hammering the broken
    // sink and switches to the stderr fallback.
    let attempts = attempts.load(Ordering::SeqCst);
    assert!(
        attempts <= 8,
        "writer should stop calling the failing sink after the threshold, saw {attempts}"
    );
    assert!(
        attempts >= 1,
        "writer should have attempted the sink at least once"
    );
}
