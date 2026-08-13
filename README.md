# xoslog

A thread-safe, robust logging library for Linux, written in pure Rust with
**zero dependencies** and **no `unsafe` code** (`#![forbid(unsafe_code)]` is
enforced at compile time).

## Features

- **Thread-safe**: callers push records onto a bounded queue; a single writer
  thread serializes all output, so records are never interleaved.
- **Non-blocking callers**: `log()` only does a timestamp + channel send.
- **Bounded memory**: the queue capacity is configurable; the
  `Backpressure::Block` policy guarantees no record loss, while
  `Backpressure::DropNewest` bounds latency and counts dropped records.
- **Robust**:
  - The writer thread survives panics and keeps serving the queue.
  - After a configurable number of consecutive write failures the writer falls
    back to stderr instead of spinning on a dead sink.
  - If the writer thread dies, callers log synchronously to stderr as a last
    resort rather than silently dropping records.
  - `flush()` and `shutdown()` give deterministic draining and teardown.
- **Size-based file rotation** with a bounded number of backups.
- **Pure-Rust timestamps** (RFC 3339, microsecond precision, configurable UTC
  offset) — no libc, no `chrono`.
- **Macros**: `log_trace!`, `log_debug!`, `log_info!`, `log_warn!`,
  `log_error!` via a process-wide global logger.

## Quick start

```rust,no_run
use xoslog::{init_default, log_info, log_error};

fn main() {
    init_default().unwrap(); // stdout sink, Info threshold
    log_info!("application starting");
    log_error!("something went wrong: {}", 42);
    if let Some(logger) = xoslog::global() {
        logger.flush();
    }
}
```

## File logging with rotation

```rust,no_run
use xoslog::{Level, LoggerBuilder};

let logger = LoggerBuilder::new()
    .level(Level::Info)
    .to_file("/var/log/app.log", 10 * 1024 * 1024, 3) // 10 MiB, keep 3 backups
    .unwrap()
    .build()
    .unwrap();

logger.info("hello from xoslog");
logger.flush();
logger.shutdown();
```

When `/var/log/app.log` exceeds the size threshold it is renamed to
`app.log.1`, older backups shift up, and the oldest (`app.log.4` here) is
dropped.

## Concurrency

`Logger` is `Clone` and `Send + Sync`; share it freely across threads.

```rust,no_run
use xoslog::{Backpressure, LoggerBuilder};
use std::thread;

let logger = LoggerBuilder::new()
    .backpressure(Backpressure::Block)
    .channel_capacity(1024)
    .to_stdout()
    .build()
    .unwrap();

let handles: Vec<_> = (0..8)
    .map(|t| {
        let logger = logger.clone();
        thread::spawn(move || {
            for i in 0..1000 {
                logger.info(format!("thread {t} message {i}"));
            }
        })
    })
    .collect();

for handle in handles {
    handle.join().unwrap();
}
logger.flush(); // guarantees every record reached the sink
```

## Backpressure policies

- `Backpressure::Block` (default): if the queue is full the caller blocks
  until the writer catches up. Nothing is ever lost.
- `Backpressure::DropNewest`: if the queue is full the newest record is
  dropped and counted; `Logger::dropped_message_count()` reports the total.

## Record format

```
2026-08-13T04:49:00.123456Z [INFO] hello world (src/main.rs:42 @ my_app)
```

Timestamps are UTC by default; use `time_offset_seconds` to shift them, e.g.
`19800` for UTC+05:30. Location tags can be disabled with
`include_location(false)`.

## Platform

Targets Linux (any platform with `std` will compile and work, but Linux is
the primary focus). The crate has no dependencies and requires no build-time
system libraries.

## License

MIT — see [LICENSE](LICENSE).
