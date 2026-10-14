use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use xoslog::{Level, LoggerBuilder, Sink};

/// A sink that records every byte written to it.
#[derive(Clone, Default)]
struct MemorySink {
    data: Arc<Mutex<Vec<u8>>>,
}

impl MemorySink {
    /// Create a sink and a handle to its recorded output.
    fn new() -> (MemorySink, Arc<Mutex<Vec<u8>>>) {
        let data = Arc::new(Mutex::new(Vec::new()));
        (
            MemorySink {
                data: Arc::clone(&data),
            },
            data,
        )
    }

    /// The recorded output as UTF-8 text.
    fn text(data: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(data.lock().unwrap().clone()).unwrap()
    }
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

/// A sink whose every write fails, counting attempts.
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
fn fan_out_to_multiple_sinks() {
    let (console, console_data) = MemorySink::new();
    let (disk, disk_data) = MemorySink::new();

    let logger = LoggerBuilder::new()
        .include_location(false)
        .add_sink(console, Level::Trace)
        .add_sink(disk, Level::Trace)
        .build()
        .unwrap();

    logger.info("hello fan-out");
    logger.error("problem");
    logger.flush();

    let console = MemorySink::text(&console_data);
    let disk = MemorySink::text(&disk_data);
    assert!(
        console.contains(" [INFO] hello fan-out"),
        "console: {console}"
    );
    assert!(console.contains(" [ERROR] problem"), "console: {console}");
    assert_eq!(console, disk, "both sinks should receive identical bytes");
}

#[test]
fn per_sink_min_level_filters() {
    let (everything, everything_data) = MemorySink::new();
    let (warnings, warnings_data) = MemorySink::new();

    let logger = LoggerBuilder::new()
        .level(Level::Trace)
        .include_location(false)
        .add_sink(everything, Level::Trace)
        .add_sink(warnings, Level::Warn)
        .build()
        .unwrap();

    logger.info("info line");
    logger.warn("warn line");
    logger.error("error line");
    logger.flush();

    let all = MemorySink::text(&everything_data);
    let only_warn = MemorySink::text(&warnings_data);

    assert!(all.contains("info line"), "all-levels sink: {all}");
    assert!(all.contains("warn line"), "all-levels sink: {all}");
    assert!(all.contains("error line"), "all-levels sink: {all}");
    assert!(
        !only_warn.contains("info line"),
        "warn-and-up sink must not receive info: {only_warn}"
    );
    assert!(
        only_warn.contains("warn line"),
        "warn-and-up sink: {only_warn}"
    );
    assert!(
        only_warn.contains("error line"),
        "warn-and-up sink: {only_warn}"
    );
}

#[test]
fn failing_sink_does_not_affect_healthy_sink() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let (healthy, healthy_data) = MemorySink::new();

    let logger = LoggerBuilder::new()
        .include_location(false)
        .add_sink(
            FailingSink {
                attempts: Arc::clone(&attempts),
            },
            Level::Trace,
        )
        .add_sink(healthy, Level::Trace)
        .build()
        .unwrap();

    for i in 0..12 {
        logger.info(format!("record {i}"));
    }
    logger.flush();

    let healthy = MemorySink::text(&healthy_data);
    assert_eq!(
        healthy.lines().count(),
        12,
        "healthy sink must receive every record despite the broken peer"
    );

    let attempts = attempts.load(Ordering::SeqCst);
    assert!(
        (1..=8).contains(&attempts),
        "broken sink should be disabled after the failure threshold, saw {attempts}"
    );
}

#[test]
fn flush_drains_every_sink() {
    let (a, a_data) = MemorySink::new();
    let (b, b_data) = MemorySink::new();

    let logger = LoggerBuilder::new()
        .include_location(false)
        .add_sink(a, Level::Trace)
        .add_sink(b, Level::Trace)
        .build()
        .unwrap();

    logger.info("drained");
    logger.flush();

    assert!(MemorySink::text(&a_data).contains("drained"));
    assert!(MemorySink::text(&b_data).contains("drained"));
}
