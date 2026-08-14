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
- **Syslog support** (RFC 3164) over the standard `/dev/log` Unix datagram
  socket, with facility selection, automatic reconnection and correct
  per-record severities.
- **Remote logging** to another Linux server via RFC 3164 syslog over UDP.
- **Structured logging**: flat JSON output (`.json()`) plus a typed
  `Field`/`FieldValue` API and a `fields!` macro for Loki/ELK/Datadog without a
  parser.
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

## Syslog

```rust,no_run
use xoslog::{Facility, Level, LoggerBuilder};

let logger = LoggerBuilder::new()
    .level(Level::Info)
    .to_syslog(Facility::Daemon) // ident derived from argv[0]
    .build()
    .unwrap();

logger.warn("low memory");
logger.flush();
```

Records are sent over the standard Linux syslog datagram socket (`/dev/log`,
`/var/run/syslog`, `/var/run/log`) as RFC 3164 packets:

```text
<PRI>TIMESTAMP HOSTNAME TAG[PID]: MSG
```

`PRI = facility * 8 + severity`, with `Level` mapped to the RFC 3164
severities (`Trace`/`Debug` -> `debug`, `Info` -> `info`, `Warn` ->
`warning`, `Error` -> `err`). The connection is lazy and re-established
automatically if the daemon restarts. For a custom socket path or identity,
construct a [`SyslogSink`] directly:

```rust,no_run
use xoslog::{Facility, LoggerBuilder, SyslogSink};

let sink = SyslogSink::new(["/run/my-syslog.sock"], Facility::Local0, "myapp");
let logger = LoggerBuilder::new().sink(sink).build().unwrap();
```

## Remote logging

Send records to a remote Linux server as RFC 3164 syslog over UDP (the
classic remote syslog transport, port 514 by convention):

```rust,no_run
use xoslog::{Facility, Level, LoggerBuilder};

let logger = LoggerBuilder::new()
    .level(Level::Info)
    .to_remote_syslog("192.0.2.10", 514, Facility::Daemon)
    .unwrap()
    .build()
    .unwrap();

logger.info("reaching across the network");
logger.flush();
```

Each record becomes a single UDP datagram (`<PRI>TIMESTAMP HOSTNAME TAG[PID]:
MSG`). UDP is connectionless and fire-and-forget, so the logger never blocks
and keeps running even if the remote host is unreachable — records may be
lost in transit, exactly as with classic UDP syslog. For a pre-resolved
address or a custom identity, construct a [`RemoteSyslogSink`] directly:

```rust,no_run
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use xoslog::{Facility, LoggerBuilder, RemoteSyslogSink};

let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 514);
let sink = RemoteSyslogSink::new(addr, Facility::Daemon, "myapp").unwrap();
let logger = LoggerBuilder::new().sink(sink).build().unwrap();
```

## Record format

```
2026-08-13T04:49:00.123456Z [INFO] hello world (src/main.rs:42 @ my_app)
```

Timestamps are UTC by default; use `time_offset_seconds` to shift them, e.g.
`19800` for UTC+05:30. Location tags can be disabled with
`include_location(false)`.

## Structured logging (JSON)

Emit each record as a single flat JSON object per line, ready for Loki, ELK
and Datadog without a server-side parser. Enable it with `.json()` and attach
typed fields to any record:

```rust,no_run
use xoslog::{Level, LoggerBuilder, LogEntry};

let logger = LoggerBuilder::new()
    .json()
    .to_file("/var/log/app.log", 10 * 1024 * 1024, 3)
    .unwrap()
    .build()
    .unwrap();

logger.log(
    LogEntry::new(Level::Info, "user login".to_string(), module_path!(), file!(), line!())
        .field("user", "alice")
        .field("attempts", 3),
);
logger.flush();
```

Output:

```json
{"ts":"2026-08-14T04:00:00.000000Z","level":"INFO","msg":"user login","file":"src/main.rs","line":42,"target":"my_app","user":"alice","attempts":3}
```

The typed field API supports strings, integers, floats, booleans and explicit
`null`:

```rust,no_run
# use xoslog::{Field, FieldValue, LogEntry, Level};
let entry = LogEntry::new(Level::Warn, "metrics".to_string(), "", "", 0)
    .with_fields(vec![
        Field::int("count", -7),
        Field::float("ratio", 0.25),
        Field::bool("healthy", false),
        Field::new("missing", FieldValue::Null),
    ]);
```

The `fields!` helper and the global macros can be combined:

```rust,no_run
# use xoslog::{fields, log_info, init_default};
# fn main() { let _ = init_default();
log_info!([fields!(user = "bob", score = 9)], "scored");
# }
```

`u64` values beyond `i64::MAX` serialize as strings to avoid truncation.
Records stay on one line: quotes, backslashes, tabs, newlines and other
control characters are JSON-escaped. Field data is only emitted by `.json()`;
plain-text sinks ignore it.

## Platform

Targets Linux (any platform with `std` will compile and work, but Linux is
the primary focus). The crate has no dependencies and requires no build-time
system libraries.

## License

MIT — see [LICENSE](LICENSE).
