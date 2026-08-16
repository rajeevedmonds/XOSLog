# Contextual/span support for xoslog

Date: 2026-08-16

## Problem

Applications want a request ID, user ID or trace ID to be carried by every log
line emitted while handling that request, without passing a context object
through every function or pulling in `tracing` as a dependency.

`xoslog` already supports structured fields: `LogEntry` carries
`Vec<Field>`, JSON sinks serialize them, and plain-text sinks ignore them. What
is missing is a mechanism to attach a set of fields to a *scope* so every log
record created inside it automatically carries those fields.

## Goals

- Attach a set of `Field`s (e.g. `request_id`, `user_id`, `trace_id`) to a
  lexical/thread-local scope.
- Every log call inside the scope — via `Logger` methods or the `log_*!`
  global macros — automatically carries the context.
- Nested scopes: an inner scope merges over its parent; the innermost value
  wins for duplicate keys.
- Records can still set the same key explicitly; the explicit value wins.
- Context may be transferred to spawned threads/tasks manually.
- Zero new dependencies, zero `unsafe` code (`#![forbid(unsafe_code)]`).

## Non-goals

- Runtime-aware async propagation (tokio task-locals, etc.). Context is
  thread-local; users transfer it across threads with the snapshot API.
- Rendering context in plain-text sinks. Fields (and therefore context) are
  only emitted by JSON sinks, consistent with the existing behavior documented
  in the README.

## Design

### Data flow

```
caller thread                     writer thread (single)
push_context([...]) -> guard
   |
log_info!(...) / logger.info(...)
   |  Logger::log() captures the calling thread's context
   |  stack, merges frames (innermost wins), then merges
   |  into entry.fields (explicit record fields win)
   v
enqueue LogEntry -------------> writer formats -> sink
```

Context is captured on the **calling thread at `log()` time** and merged into
`LogEntry.fields` before the record is enqueued. The writer thread never reads
thread-local storage.

### New module `context`

A new `src/context.rs` module, re-exported from the crate root:

- `push_context(fields) -> ContextGuard` — pushes a frame onto the current
  thread's context stack; the returned guard pops it on drop.
- `ContextGuard` — RAII guard; `!Send`, `!Sync` so it cannot escape the
  thread. Dropping it restores the previous stack state.
- `capture_context() -> ContextSnapshot` — snapshot of the current thread's
  *merged* context as a list of fields.
- `ContextSnapshot::enter(&self) -> ContextGuard` — pushes the snapshot as a
  frame onto the current thread's stack (used to transfer context into spawned
  threads/tasks).
- `current_context() -> Vec<Field>` — read-only merged view of the current
  thread's context.

All functions operate on a `thread_local!` stack of `Vec<Field>` frames
guarded by a `RefCell`. No `unsafe`.

### Merging semantics

At `log()` time the context stack is merged into a flat, deduplicated list:

1. Frames are merged outer-to-inner; for duplicate keys the **innermost**
   frame's value wins.
2. The merged context is combined with the record's own fields such that the
   record's **explicit fields win** over context for duplicate keys.
3. Context fields are appended after the record's own fields (stable order,
   no shuffle of existing fields).

The merged fields flow through the existing pipeline unchanged: JSON sinks
emit them, plain-text sinks ignore them, and the emergency stderr fallback
emits them in JSON mode (it reuses the same `LogEntry`; in plain-text mode it
ignores them like any plain-text sink).

### API surface (public)

```rust
// src/context.rs, re-exported from lib.rs
pub struct ContextGuard { /* opaque, !Send, !Sync */ }
pub struct ContextSnapshot { /* Vec<Field> */ }

pub fn push_context(fields: impl IntoIterator<Item = Field>) -> ContextGuard;
pub fn capture_context() -> ContextSnapshot;
pub fn current_context() -> Vec<Field>;

impl ContextSnapshot {
    pub fn enter(&self) -> ContextGuard;
}
```

### Integration point

`Logger::log()` (in `src/logger.rs`) gains a single call before enqueueing:

```rust
let ctx = crate::context::current_context();
if !ctx.is_empty() {
    entry.merge_context(ctx);
}
```

A helper `LogEntry::merge_context(self, context: Vec<Field>) -> LogEntry` in
`src/entry.rs` implements the deduplication (explicit fields win, context
appended). `LogEntry::new` and the convenience methods are unchanged.

### Usage example

```rust
use xoslog::{init_default, log_info, push_context};

fn main() {
    init_default().unwrap();

    let _guard = push_context(vec![
        xoslog::Field::str("request_id", "abc-123"),
        xoslog::Field::str("user_id", "u42"),
    ]);
    log_info!("handling request"); // JSON output carries request_id, user_id

    // Transferred context:
    let snapshot = xoslog::capture_context();
    std::thread::spawn(move || {
        let _g = snapshot.enter();
        log_info!("worker thread"); // also carries request_id, user_id
    });
}
```

## Testing

Unit tests in `src/context.rs`:

- `push_context` returns a guard; the stack is empty before/after.
- Nested guards: inner duplicate key overrides outer; popping the inner guard
  restores the outer value.
- `current_context()` returns the merged, deduplicated view.
- `capture_context()` snapshot is independent of later mutations.
- `ContextSnapshot::enter()` pushes the snapshot onto a fresh stack.
- `ContextGuard` is `!Send` (compile-time, via a static assertion if cheap).

Integration tests in `tests/context.rs`:

- JSON sink: records inside a scope carry context keys; records outside do not.
- Plain-text sink: context never appears.
- Explicit record field wins over context for the same key.
- Global macros (`log_info!`) inside a scope carry context.
- Spawned thread with `snapshot.enter()` logs with transferred context.
- Context is captured at `log()` time: pushing/popping after a record is
  enqueued does not alter already-written records (verified via `flush`).

## Documentation

- README: new `## Contextual logging (spans)` section with usage examples,
  precedence rules and the thread-transfer note.
- Doc comments on all new public items.

## Files touched

- `src/context.rs` (new)
- `src/lib.rs` (re-export module)
- `src/logger.rs` (capture context in `Logger::log`)
- `src/entry.rs` (`LogEntry::merge_context`)
- `tests/context.rs` (new)
- `README.md` (new section)
