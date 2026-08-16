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
- **Contextual logging (spans)**: a thread-local scope API (`push_context`,
  `capture_context`) that attaches request/user/trace IDs to every record
  emitted inside a scope, with nesting and thread-transfer support.
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

## Contextual logging (spans)

Attach a request ID, user ID or trace ID to a scope so every log line emitted
inside it automatically carries that context — no `tracing` dependency and no
need to thread a context object through every function.

`push_context` pushes a scope onto a thread-local stack and returns a guard
that pops it when dropped:

```rust,no_run
use xoslog::{init_default, log_info, push_context, Field};

fn main() {
    init_default().unwrap();

    let _guard = push_context([
        Field::str("request_id", "abc-123"),
        Field::str("user_id", "u42"),
    ]);
    log_info!("handling request"); // carries request_id and user_id
}
```

With a JSON sink the output includes the context keys:

```json
{"ts":"2026-08-14T04:00:00.000000Z","level":"INFO","msg":"handling request","file":"src/main.rs","line":12,"target":"my_app","request_id":"abc-123","user_id":"u42"}
```

Rules:

- **Nesting**: scopes nest; an inner scope merges over its parent and wins for
  duplicate keys. Popping the inner guard restores the outer value.
- **Precedence**: fields set explicitly on a record (via `LogEntry::field` or
  `fields!`) always win over context with the same key.
- **Format**: context merges into the record's structured fields, so only JSON
  sinks emit it; plain-text sinks ignore it, like any other field.
- **Capture at `log()` time**: context is snapshotted when the record is
  enqueued, so later scope changes never rewrite already-written lines.

Context is thread-local and does not propagate to spawned threads or async
tasks automatically. Transfer it with a snapshot:

```rust,no_run
# use xoslog::{push_context, capture_context, Field};
# fn main() {
let _guard = push_context([Field::str("request_id", "abc-123")]);
let snapshot = capture_context();

std::thread::spawn(move || {
    let _guard = snapshot.enter(); // worker logs now carry the same context
    // ...
});
# }
```

For a read-only view of the current merged context, call `current_context()`.

## Building and installing on Linux

`xoslog` is a plain Cargo crate with **zero dependencies** and no build-time
system libraries, so building it requires nothing but the Rust toolchain.

### Prerequisites

Install the Rust toolchain (stable). On Debian/Ubuntu:

```bash
# Install curl and a C linker/compiler toolchain
sudo apt install -y curl build-essential

# Install Rust via rustup (unattended)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal

# Load cargo/rustc into the current shell
source "$HOME/.cargo/env"
```

On Fedora/RHEL:

```bash
sudo dnf install -y curl gcc
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"
```

Alternatively, use your distro's packaged toolchain (`sudo apt install cargo`
or `sudo dnf install cargo`) — any recent stable Rust works; the crate targets
edition 2021.

### Build and test

```bash
# Debug build (artifact: target/debug/libxoslog.rlib)
cargo build

# Optimized release build (artifact: target/release/libxoslog.rlib)
cargo build --release

# Run the full test suite, including integration and doc tests
cargo test --release

# Build the API documentation (target/doc/xoslog/index.html)
cargo doc --no-deps
```

The release profile in `Cargo.toml` enables `opt-level = 3`, LTO and
`codegen-units = 1`, so the resulting `.rlib` is small and fast.

### Make the library available system-wide

Rust builds each dependency from source, so a compiled `.rlib` alone is not
how Rust projects normally consume a library. There are two supported ways to
make `xoslog` available system-wide on a Linux machine.

#### 1. Publish to crates.io (recommended for multi-project use)

Publishing makes the crate fetchable by any project on the machine using plain
Cargo. This requires a [crates.io](https://crates.io) account.

```bash
cargo login        # paste your crates.io API token
cargo publish      # uploads xoslog 0.1.0
```

Any other project can then add it as a dependency:

```toml
[dependencies]
xoslog = "0.1"
```

or, inside a project directory:

```bash
cargo add xoslog
```

#### 2. Install the compiled artifacts locally (offline, single machine)

If you cannot publish, copy the build artifacts, crate sources and docs into
system-wide locations and consume the crate from the filesystem.

```bash
# Install the built library into a system-wide location
sudo install -d /usr/local/lib/rust/xoslog
sudo install -m 644 target/release/libxoslog.rlib target/release/libxoslog.d \
  /usr/local/lib/rust/xoslog/

# Install the crate sources (needed for path-based dependencies)
sudo mkdir -p /usr/local/src/xoslog
sudo cp -r Cargo.toml src /usr/local/src/xoslog/

# Install the API documentation
sudo install -d /usr/local/share/doc/xoslog
sudo cp -r target/doc/xoslog/* /usr/local/share/doc/xoslog/
```

Any project on the machine can then depend on the installed copy:

```toml
[dependencies]
xoslog = { path = "/usr/local/src/xoslog" }
```

and a standalone program can link against the installed `.rlib` directly:

```bash
rustc --edition 2021 --extern xoslog=target/release/libxoslog.rlib my_program.rs
```

### Smoke test

Verify that a project on this machine can use the library. Create a scratch
project, add `xoslog` as a dependency, and run it:

```bash
cargo new --bin smoke
cd smoke
cargo add xoslog
```

Replace `src/main.rs` with:

```rust
use xoslog::{init_default, log_info};

fn main() {
    init_default().unwrap(); // stdout sink, Info threshold
    log_info!("xoslog is ready for system-wide use");
    if let Some(logger) = xoslog::global() {
        logger.flush();
    }
}
```

Then run it:

```bash
cargo run
```

You should see a single line like:

```text
2026-08-13T04:49:00.123456Z [INFO] xoslog is ready for system-wide use (src/main.rs:4 @ smoke)
```

## Platform

Targets Linux (any platform with `std` will compile and work, but Linux is
the primary focus). The crate has no dependencies and requires no build-time
system libraries.

## License

MIT — see [LICENSE](LICENSE).
