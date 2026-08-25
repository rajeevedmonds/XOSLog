use std::thread;
use std::time::Duration;

use xoslog::{log_error, log_info, log_warn, Backpressure, Level, LoggerBuilder};

fn main() {
    // 1) File-backed logger with rotation: max 1 MiB per file, keep 5 backups.
    let logger = LoggerBuilder::new()
        .level(Level::Info)
        .backpressure(Backpressure::Block)
        .channel_capacity(4096)
        .time_offset_seconds(0) // UTC
        .to_file("/tmp/xoslog-example.log", 1024 * 1024, 5)
        .unwrap()
        .build()
        .unwrap();

    // 2) Convenience methods on the Logger.
    logger.info("logger created");
    logger.warn("rotation enabled, max 1 MiB");

    // 3) Log from many threads at once.
    let mut handles = Vec::new();
    for t in 0..4 {
        let logger = logger.clone();
        handles.push(thread::spawn(move || {
            for i in 0..10 {
                logger.info(format!("thread {t} iteration {i}"));
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }

    // 4) Block until everything enqueued so far is on disk.
    logger.flush();
    println!(
        "wrote {} records",
        std::fs::read_to_string("/tmp/xoslog-example.log")
            .map(|s| s.lines().count())
            .unwrap_or(0)
    );

    // 5) Global logger + macros.
    let global = LoggerBuilder::new()
        .level(Level::Info)
        .to_stderr()
        .build()
        .unwrap();
    let _ = xoslog::set_global(global);

    log_info!("application is shutting down");
    log_warn!("cache eviction took {} ms", 42);
    log_error!("failed to contact upstream service");

    if let Some(g) = xoslog::global() {
        g.flush();
    }

    // 6) Deterministic teardown (flush + join writer thread).
    let clone = logger.clone();
    clone.shutdown();

    // Demonstrate the shutdown semantics: this is a no-op now.
    logger.info("ignored after shutdown");
    thread::sleep(Duration::from_millis(10));
    println!("done");
}
