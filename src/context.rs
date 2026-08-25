//! Thread-local contextual/span support.
//!
//! Attach structured fields (request IDs, user IDs, trace IDs, ...) to a
//! scope so every log record emitted inside it automatically carries them,
//! without threading a context object through every function and without a
//! `tracing` dependency.
//!
//! Context lives in a per-thread stack of frames. [`push_context`] pushes a
//! frame and returns a [`ContextGuard`] that pops it on drop. Frames nest: an
//! inner frame merges over its parent, and for duplicate keys the innermost
//! frame wins. [`Logger::log`] captures the calling thread's merged context
//! and attaches it to every record; explicit record fields always win over
//! context.
//!
//! Context is thread-local and does not propagate to spawned threads or async
//! tasks automatically. Use [`capture_context`] to snapshot the current
//! context and [`ContextSnapshot::enter`] to re-apply it on another thread.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::entry::Field;

/// A single frame on a thread's context stack.
struct Frame {
    /// Unique identifier used to pop the exact frame on guard drop.
    id: usize,
    /// The fields this scope contributes.
    fields: Vec<Field>,
}

thread_local! {
    /// The current thread's context frames, outermost first.
    static CONTEXT_STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// Source of unique frame identifiers (per process).
static NEXT_FRAME_ID: AtomicUsize = AtomicUsize::new(1);

/// An RAII guard that removes a context scope when dropped.
///
/// Returned by [`push_context`] and [`ContextSnapshot::enter`]. The guard is
/// `!Send` and `!Sync`: a context scope is bound to the thread that created it
/// and must not cross thread boundaries. Dropping it removes exactly the frame
/// it pushed, so guards may be dropped in any order.
#[must_use = "dropping the guard immediately discards the context scope"]
pub struct ContextGuard {
    id: usize,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl ContextGuard {
    fn new(id: usize) -> ContextGuard {
        ContextGuard {
            id,
            _not_send_or_sync: PhantomData,
        }
    }
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        CONTEXT_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if let Some(pos) = stack.iter().position(|frame| frame.id == self.id) {
                stack.remove(pos);
            }
        });
    }
}

/// A snapshot of the current thread's merged context.
///
/// Produced by [`capture_context`] and re-applied on another thread with
/// [`ContextSnapshot::enter`]. Unlike [`ContextGuard`] it is `Send + Sync`, so
/// it can be moved into a spawned thread or stored alongside an async task.
#[derive(Debug, Clone, Default)]
pub struct ContextSnapshot {
    fields: Vec<Field>,
}

impl ContextSnapshot {
    /// Push this snapshot onto the current thread's context stack.
    ///
    /// The returned guard pops the snapshot when dropped. This is how context
    /// is transferred across thread boundaries:
    ///
    /// ```
    /// # use xoslog::{push_context, capture_context, current_context, Field};
    /// # let _guard = push_context([Field::str("request_id", "abc-123")]);
    /// let snapshot = capture_context();
    /// let handle = std::thread::spawn(move || {
    ///     let _guard = snapshot.enter();
    ///     assert_eq!(current_context().len(), 1);
    /// });
    /// handle.join().unwrap();
    /// ```
    pub fn enter(&self) -> ContextGuard {
        push_context(self.fields.iter().cloned())
    }
}

/// Push a context frame onto the current thread's stack.
///
/// Every log record created while the returned guard is alive automatically
/// carries `fields` (merged with any outer scopes; the innermost value wins
/// for duplicate keys). The frame is removed when the guard is dropped.
///
/// ```
/// # use xoslog::{push_context, log_info, init_default, Field};
/// # fn main() { let _ = init_default();
/// let _guard = push_context([
///     Field::str("request_id", "abc-123"),
///     Field::str("user_id", "u42"),
/// ]);
/// log_info!("handling request"); // carries request_id and user_id
/// # }
/// ```
#[must_use = "the returned guard must be kept alive for the scope to last"]
pub fn push_context(fields: impl IntoIterator<Item = Field>) -> ContextGuard {
    let id = NEXT_FRAME_ID.fetch_add(1, Ordering::Relaxed);
    let frame = Frame {
        id,
        fields: fields.into_iter().collect(),
    };
    CONTEXT_STACK.with(|stack| {
        stack.borrow_mut().push(frame);
    });
    ContextGuard::new(id)
}

/// Snapshot the current thread's merged context.
///
/// The snapshot is independent of later mutations to the current context and
/// can be moved to another thread and re-applied with
/// [`ContextSnapshot::enter`].
#[must_use]
pub fn capture_context() -> ContextSnapshot {
    ContextSnapshot {
        fields: current_context(),
    }
}

/// The current thread's merged context as a deduplicated list of fields.
///
/// Frames are merged outermost first; for duplicate keys the innermost frame's
/// value wins. An empty list means no context is active.
#[must_use]
pub fn current_context() -> Vec<Field> {
    CONTEXT_STACK.with(|stack| {
        let stack = stack.borrow();
        if stack.is_empty() {
            return Vec::new();
        }
        let mut merged: Vec<Field> = Vec::new();
        for frame in stack.iter() {
            for field in &frame.fields {
                match merged.iter_mut().find(|existing| existing.key == field.key) {
                    Some(existing) => *existing = field.clone(),
                    None => merged.push(field.clone()),
                }
            }
        }
        merged
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn context_is_empty_by_default() {
        assert!(current_context().is_empty());
    }

    #[test]
    fn guard_removes_scope_on_drop() {
        assert!(current_context().is_empty());
        {
            let _guard = push_context([Field::str("request_id", "abc")]);
            assert_eq!(key(&["request_id"]), context_keys());
        }
        assert!(current_context().is_empty());
    }

    #[test]
    fn nested_scopes_merge_innermost_wins() {
        let outer = push_context([Field::str("request_id", "outer"), Field::str("user", "alice")]);
        {
            let inner = push_context([Field::str("request_id", "inner")]);
            let ctx = current_context();
            assert_eq!(ctx.len(), 2);
            assert!(ctx.iter().any(|f| f.key == "request_id" && matches!(&f.value, crate::entry::FieldValue::Str(v) if v == "inner")));
            assert!(ctx.iter().any(|f| f.key == "user" && matches!(&f.value, crate::entry::FieldValue::Str(v) if v == "alice")));
            drop(inner);
        }
        // Outer frame restored after the inner guard is dropped.
        let ctx = current_context();
        assert_eq!(ctx.len(), 2);
        assert!(ctx.iter().any(|f| f.key == "request_id" && matches!(&f.value, crate::entry::FieldValue::Str(v) if v == "outer")));
        drop(outer);
        assert!(current_context().is_empty());
    }

    #[test]
    fn guards_may_drop_out_of_order() {
        let first = push_context([Field::str("a", "1")]);
        let second = push_context([Field::str("b", "2")]);
        // Dropping the outer guard first must not corrupt the stack.
        drop(first);
        let ctx = current_context();
        assert_eq!(ctx.len(), 1);
        assert!(ctx.iter().any(|f| f.key == "b"));
        drop(second);
        assert!(current_context().is_empty());
    }

    #[test]
    fn snapshot_is_independent_and_reenterable() {
        let _guard = push_context([Field::str("request_id", "abc")]);
        let snapshot = capture_context();
        // Mutating the live context after capture must not affect the snapshot.
        let inner = push_context([Field::str("extra", "x")]);
        assert_eq!(snapshot.fields.len(), 1);

        let entered = snapshot.enter();
        let ctx = current_context();
        // Live stack: extra frame + re-entered snapshot frame.
        assert!(ctx.iter().any(|f| f.key == "extra"));
        assert!(ctx.iter().any(|f| f.key == "request_id"));
        drop(entered);
        drop(inner);
        let _guard = push_context([Field::str("request_id", "zzz")]);
        let mut ctx = current_context();
        assert!(ctx.iter().any(|f| f.key == "request_id" && matches!(&f.value, crate::entry::FieldValue::Str(v) if v == "zzz")));
        // Re-applying the old snapshot on a fresh (empty) stack works too.
        let reentered = snapshot.enter();
        ctx = current_context();
        assert!(ctx.iter().any(|f| f.key == "request_id" && matches!(&f.value, crate::entry::FieldValue::Str(v) if v == "abc")));
        assert!(ctx.iter().any(|f| f.key == "request_id"));
        drop(reentered);
    }

    #[test]
    fn snapshot_can_move_to_another_thread() {
        let _guard = push_context([Field::str("request_id", "abc")]);
        let snapshot = capture_context();
        let handle = std::thread::spawn(move || {
            assert!(current_context().is_empty());
            let _guard = snapshot.enter();
            let ctx = current_context();
            assert_eq!(ctx.len(), 1);
            assert!(ctx.iter().any(|f| f.key == "request_id"));
        });
        handle.join().unwrap();
        // Original thread unaffected.
        assert_eq!(current_context().len(), 1);
    }

    fn context_keys() -> Vec<String> {
        current_context().into_iter().map(|f| f.key).collect()
    }
}
