use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;
use triomphe::Arc;

use crate::{DropGuard, DropGuardRef, WaitForCancellationFuture};

/// A one-shot cancellation signal shared by all clones.
///
/// Dropping a token does not cancel it. There are no child tokens or resets.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    inner: Arc<State>,
}

#[derive(Debug, Default)]
struct State {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    /// Creates an independent, uncancelled token. Equivalent to `default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation and wakes all current waiters.
    ///
    /// Cancellation is permanent. Calling this more than once is harmless.
    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            self.inner.notify.notify_waiters();
        }
    }

    /// Checks cancellation using a single atomic load, without locking.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Takes ownership of this token and cancels it when the guard is dropped.
    ///
    /// Creating a guard does not allocate or clone the token. Call
    /// [`DropGuard::disarm`] to recover the token without cancelling it.
    /// Other guards for the same cancellation state are unaffected.
    #[inline]
    pub fn drop_guard(self) -> DropGuard {
        DropGuard::new(self)
    }

    /// Borrows this token and cancels it when the guard is dropped.
    ///
    /// This does not allocate or change the token's reference count. Call
    /// [`DropGuardRef::disarm`] to remove the guard without cancelling it.
    #[inline]
    pub fn drop_guard_ref(&self) -> DropGuardRef<'_> {
        DropGuardRef::new(self)
    }

    /// Waits for cancellation, returning immediately if already cancelled.
    ///
    /// Cancel safe: dropping this future only unregisters this waiter.
    pub fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        // notify_waiters() covers futures created before the notification,
        // even if not yet polled. Create first, then check the sticky flag.
        WaitForCancellationFuture::new(self, self.inner.notify.notified())
    }
}
